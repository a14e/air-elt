use ahash::AHashMap;
use air_elt_expr_parse::Parser;
use air_elt_expr_types::limits::MAX_EXPR_DEPTH;

use crate::context::ExpressionContext;
use crate::error::ExprError;

pub struct ConfigExprPatcher {
    trie: PatternTrie,
}

impl ConfigExprPatcher {
    pub fn create(patterns: &[&str]) -> Result<Self, ExprError> {
        let trie = PatternTrie::build(patterns)?;
        Ok(Self { trie })
    }

    pub fn patch(
        &self,
        table: &mut toml::Table,
        context: &ExpressionContext,
    ) -> Result<(), ExprError> {
        let parser = Parser::create_comptime();
        self.trie.root.walk_table(table, &parser, context)
    }

    pub fn patch_table(
        &self,
        table: &mut toml::Table,
        context: &ExpressionContext,
    ) -> Result<(), ExprError> {
        let parser = Parser::create_comptime();
        let mut value = toml::Value::Table(std::mem::take(table));
        TrieNode::patch_subtree(&mut value, &parser, context, 0)?;
        if let toml::Value::Table(patched) = value {
            *table = patched;
        }
        Ok(())
    }
}

struct PatternTrie {
    root: TrieNode,
}

impl PatternTrie {
    fn build(patterns: &[&str]) -> Result<Self, ExprError> {
        let mut root = TrieNode::default();
        for pattern in patterns {
            let segments = Self::parse_pattern(pattern)?;
            root.insert_segments(&segments);
        }
        Ok(Self { root })
    }

    fn parse_pattern(pattern: &str) -> Result<Vec<Segment>, ExprError> {
        let mut segments = Vec::new();
        let mut remaining = pattern;

        while !remaining.is_empty() {
            if remaining.starts_with("[*]") {
                segments.push(Segment::AnyIndex);
                remaining = &remaining[3..];
                if remaining.starts_with('.') {
                    remaining = &remaining[1..];
                }
            } else if remaining.starts_with('*') {
                segments.push(Segment::AnyKey);
                remaining = &remaining[1..];
                if remaining.starts_with('.') {
                    remaining = &remaining[1..];
                }
            } else {
                let end = remaining.find(['.', '[']).unwrap_or(remaining.len());
                if end == 0 {
                    return Err(ExprError::InvalidPattern(format!(
                        "empty segment in '{pattern}'"
                    )));
                }
                segments.push(Segment::Exact(remaining[..end].to_owned()));
                remaining = &remaining[end..];
                if remaining.starts_with('.') {
                    remaining = &remaining[1..];
                }
            }
        }

        Ok(segments)
    }
}

#[derive(Default)]
struct TrieNode {
    exact_children: AHashMap<String, TrieNode>,
    wildcard_child: Option<Box<TrieNode>>,
    array_wildcard_child: Option<Box<TrieNode>>,
    is_subtree_terminal: bool,
}

impl TrieNode {
    fn insert_segments(&mut self, segments: &[Segment]) {
        if segments.is_empty() {
            self.is_subtree_terminal = true;
            return;
        }

        let (segment, rest) = (&segments[0], &segments[1..]);

        let child = match segment {
            Segment::Exact(name) => self.exact_children.entry(name.clone()).or_default(),
            Segment::AnyKey => self
                .wildcard_child
                .get_or_insert_with(|| Box::new(TrieNode::default())),
            Segment::AnyIndex => self
                .array_wildcard_child
                .get_or_insert_with(|| Box::new(TrieNode::default())),
        };

        if rest.is_empty() {
            child.is_subtree_terminal = true;
        } else {
            child.insert_segments(rest);
        }
    }

    fn walk_table(
        &self,
        table: &mut toml::Table,
        parser: &Parser,
        context: &ExpressionContext,
    ) -> Result<(), ExprError> {
        let keys: Vec<String> = table.keys().cloned().collect();
        for key in keys {
            let child_node = self
                .exact_children
                .get(key.as_str())
                .or(self.wildcard_child.as_deref());

            if let Some(child) = child_node {
                if child.is_subtree_terminal {
                    if let Some(value) = table.get_mut(&key) {
                        Self::patch_subtree(value, parser, context, 0)?;
                    }
                } else if let Some(value) = table.get_mut(&key) {
                    child.walk_value(value, parser, context)?;
                }
            }
        }
        Ok(())
    }

    fn walk_value(
        &self,
        value: &mut toml::Value,
        parser: &Parser,
        context: &ExpressionContext,
    ) -> Result<(), ExprError> {
        match value {
            toml::Value::Table(t) => self.walk_table(t, parser, context),
            toml::Value::Array(arr) => {
                if let Some(array_child) = self.array_wildcard_child.as_deref() {
                    for element in arr.iter_mut() {
                        if array_child.is_subtree_terminal {
                            Self::patch_subtree(element, parser, context, 0)?;
                        } else if let toml::Value::Table(t) = element {
                            array_child.walk_table(t, parser, context)?;
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn patch_subtree(
        value: &mut toml::Value,
        parser: &Parser,
        context: &ExpressionContext,
        depth: usize,
    ) -> Result<(), ExprError> {
        if depth > MAX_EXPR_DEPTH {
            return Err(ExprError::InvalidPattern(
                "TOML nesting too deep for expression patching".into(),
            ));
        }
        match value {
            toml::Value::String(s) => {
                if parser.is_expr(s) {
                    let program = parser.parse(s)?;
                    let result = context.evaluate_const(&program)?;
                    *value = result
                        .to_toml()
                        .unwrap_or_else(|| toml::Value::String(format!("{result:?}")));
                }
            }
            toml::Value::Table(t) => {
                let keys: Vec<String> = t.keys().cloned().collect();
                for k in keys {
                    if let Some(v) = t.get_mut(&k) {
                        Self::patch_subtree(v, parser, context, depth + 1)?;
                    }
                }
            }
            toml::Value::Array(arr) => {
                for v in arr.iter_mut() {
                    Self::patch_subtree(v, parser, context, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
enum Segment {
    Exact(String),
    AnyKey,
    AnyIndex,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use air_elt_expr_funcs::FunctionRegistry;

    use super::*;

    #[test]
    fn parse_pattern_simple() {
        let segments = PatternTrie::parse_pattern("sources[*].config").unwrap();
        assert_eq!(
            segments,
            vec![
                Segment::Exact("sources".to_owned()),
                Segment::AnyIndex,
                Segment::Exact("config".to_owned()),
            ]
        );
    }

    #[test]
    fn parse_pattern_with_wildcard() {
        let segments = PatternTrie::parse_pattern("flow.*.mapping.*.default").unwrap();
        assert_eq!(
            segments,
            vec![
                Segment::Exact("flow".to_owned()),
                Segment::AnyKey,
                Segment::Exact("mapping".to_owned()),
                Segment::AnyKey,
                Segment::Exact("default".to_owned()),
            ]
        );
    }

    #[test]
    fn trie_marks_subtree_terminal() {
        let trie = PatternTrie::build(&["sources[*].config"]).unwrap();
        let sources = trie.root.exact_children.get("sources").unwrap();
        let array_child = sources.array_wildcard_child.as_ref().unwrap();
        let config = array_child.exact_children.get("config").unwrap();
        assert!(config.is_subtree_terminal);
    }

    fn test_context() -> ExpressionContext {
        ExpressionContext::create(
            Arc::new(FunctionRegistry::with_builtins()),
            Path::new("/tmp"),
        )
    }

    #[test]
    fn patches_matching_paths() {
        let patcher = ConfigExprPatcher::create(&[
            "sinks[*].config",
            "sources[*].config",
            "storages[*].config",
        ])
        .unwrap();

        let ctx = test_context();

        let toml_str = r#"
            [[sources]]
            name = "src"
            type = "postgres"

            [sources.config]
            url = "concat('pg://', 'localhost')"
            port = 5432

            [[sinks]]
            name = "snk"
            type = "postgres"

            [sinks.config]
            url = "plain_string"
        "#;
        let mut root: toml::Table = toml::from_str(toml_str).unwrap();

        patcher.patch(&mut root, &ctx).unwrap();

        let sources = root.get("sources").unwrap().as_array().unwrap();
        let src_config = sources[0].get("config").unwrap().as_table().unwrap();
        assert_eq!(
            src_config.get("url").unwrap().as_str().unwrap(),
            "pg://localhost"
        );
        assert_eq!(src_config.get("port").unwrap().as_integer().unwrap(), 5432);

        let sinks = root.get("sinks").unwrap().as_array().unwrap();
        let snk_config = sinks[0].get("config").unwrap().as_table().unwrap();
        assert_eq!(
            snk_config.get("url").unwrap().as_str().unwrap(),
            "plain_string"
        );
    }

    #[test]
    fn skips_non_matching_paths() {
        let patcher = ConfigExprPatcher::create(&["sources[*].config"]).unwrap();
        let ctx = test_context();

        let toml_str = r#"
            [[sources]]
            name = "concat('should', 'not', 'eval')"
            type = "postgres"
            [sources.config]
            url = "add(1, 2)"
        "#;
        let mut root: toml::Table = toml::from_str(toml_str).unwrap();

        patcher.patch(&mut root, &ctx).unwrap();

        let sources = root.get("sources").unwrap().as_array().unwrap();
        assert_eq!(
            sources[0].get("name").unwrap().as_str().unwrap(),
            "concat('should', 'not', 'eval')"
        );
        let config = sources[0].get("config").unwrap().as_table().unwrap();
        assert_eq!(config.get("url").unwrap().as_integer().unwrap(), 3);
    }

    #[test]
    fn patches_nested_config_tables() {
        let patcher = ConfigExprPatcher::create(&["sources[*].config"]).unwrap();
        let ctx = test_context();

        let toml_str = r#"
            [[sources]]
            name = "src"
            type = "postgres"

            [sources.config]
            url = "plain"

            [sources.config.nested]
            key = "add(10, 20)"
        "#;
        let mut root: toml::Table = toml::from_str(toml_str).unwrap();

        patcher.patch(&mut root, &ctx).unwrap();

        let sources = root.get("sources").unwrap().as_array().unwrap();
        let nested = sources[0]
            .get("config")
            .unwrap()
            .as_table()
            .unwrap()
            .get("nested")
            .unwrap()
            .as_table()
            .unwrap();
        assert_eq!(nested.get("key").unwrap().as_integer().unwrap(), 30);
    }

    #[test]
    fn patches_interpolation() {
        let patcher = ConfigExprPatcher::create(&["sources[*].config"]).unwrap();
        let ctx = test_context();

        let toml_str = r#"
            [[sources]]
            name = "src"
            type = "postgres"

            [sources.config]
            url = "prefix_{1 + 2}_suffix"
        "#;
        let mut root: toml::Table = toml::from_str(toml_str).unwrap();

        patcher.patch(&mut root, &ctx).unwrap();

        let sources = root.get("sources").unwrap().as_array().unwrap();
        let config = sources[0].get("config").unwrap().as_table().unwrap();
        assert_eq!(
            config.get("url").unwrap().as_str().unwrap(),
            "prefix_3_suffix"
        );
    }

    #[test]
    fn empty_patterns_noop() {
        let patcher = ConfigExprPatcher::create(&[]).unwrap();
        let ctx = test_context();

        let toml_str = r#"
            [[sources]]
            name = "add(1, 2)"
            [sources.config]
            url = "add(3, 4)"
        "#;
        let mut root: toml::Table = toml::from_str(toml_str).unwrap();
        let original = root.clone();

        patcher.patch(&mut root, &ctx).unwrap();

        assert_eq!(root, original);
    }

    #[test]
    fn invalid_expression_propagates_error() {
        let patcher = ConfigExprPatcher::create(&["sources[*].config"]).unwrap();
        let ctx = test_context();

        let toml_str = r#"
            [[sources]]
            name = "src"
            type = "postgres"
            [sources.config]
            url = "nonexistent_func()"
        "#;
        let mut root: toml::Table = toml::from_str(toml_str).unwrap();

        let result = patcher.patch(&mut root, &ctx);
        assert!(result.is_err());
    }

    #[test]
    fn patches_arrays_inside_config() {
        let patcher = ConfigExprPatcher::create(&["sources[*].config"]).unwrap();
        let ctx = test_context();

        let toml_str = r#"
            [[sources]]
            name = "src"
            type = "postgres"
            [sources.config]
            tags = ["concat('a', 'b')", "plain"]
        "#;
        let mut root: toml::Table = toml::from_str(toml_str).unwrap();

        patcher.patch(&mut root, &ctx).unwrap();

        let sources = root.get("sources").unwrap().as_array().unwrap();
        let config = sources[0].get("config").unwrap().as_table().unwrap();
        let tags = config.get("tags").unwrap().as_array().unwrap();
        assert_eq!(tags[0].as_str().unwrap(), "ab");
        assert_eq!(tags[1].as_str().unwrap(), "plain");
    }
}
