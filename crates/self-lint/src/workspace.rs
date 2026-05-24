use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrateCategory {
    Foundation,
    CommonsTesting,
    CommonsDb,
    Monitoring,
    Expression,
    Core,
    Source,
    Sink,
    Storage,
    App,
    SelfLint,
}

#[derive(Debug)]
pub struct CrateInfo {
    pub name: String,
    pub path: PathBuf,
    pub relative_path: String,
    pub category: Option<CrateCategory>,
    pub dependencies: Vec<String>,
    #[allow(dead_code)]
    pub dev_dependencies: Vec<String>,
    pub has_lib: bool,
    pub doctest_disabled: bool,
    pub autotests_disabled: bool,
    pub has_test_all: bool,
    pub has_tests_directory: bool,
}

#[derive(Debug)]
pub struct Workspace {
    #[allow(dead_code)]
    pub root: PathBuf,
    pub version: String,
    pub crates: BTreeMap<String, CrateInfo>,
}

impl Workspace {
    pub fn load(root: &Path) -> Result<Self, String> {
        let root_toml_path = root.join("Cargo.toml");
        let root_content = std::fs::read_to_string(&root_toml_path)
            .map_err(|error| format!("cannot read {}: {error}", root_toml_path.display()))?;
        let root_doc: toml::Value = toml::from_str(&root_content)
            .map_err(|error| format!("cannot parse {}: {error}", root_toml_path.display()))?;

        let version = root_doc
            .get("workspace")
            .and_then(|workspace| workspace.get("package"))
            .and_then(|package| package.get("version"))
            .and_then(|version| version.as_str())
            .ok_or("missing [workspace.package].version")?
            .to_string();

        let members = root_doc
            .get("workspace")
            .and_then(|workspace| workspace.get("members"))
            .and_then(|members| members.as_array())
            .ok_or("missing [workspace].members")?;

        let mut crates = BTreeMap::new();
        for member in members {
            let relative_path = member.as_str().ok_or("member is not a string")?.to_string();
            let crate_path = root.join(&relative_path);
            let info = parse_crate(root, &crate_path, &relative_path)?;
            crates.insert(info.name.clone(), info);
        }

        Ok(Self {
            root: root.to_path_buf(),
            version,
            crates,
        })
    }

    pub fn crates_by_category(&self, category: CrateCategory) -> Vec<&CrateInfo> {
        self.crates
            .values()
            .filter(|crate_info| crate_info.category == Some(category))
            .collect()
    }
}

// Order matters: exact matches before prefix matches, narrower prefixes before wider ones.
const CLASSIFICATION_RULES: &[(&str, CrateCategory)] = &[
    ("crates/types", CrateCategory::Foundation),
    ("crates/commons/lib", CrateCategory::Foundation),
    ("crates/commons/testing", CrateCategory::CommonsTesting),
    ("crates/commons/", CrateCategory::CommonsDb),
    ("crates/monitoring", CrateCategory::Monitoring),
    ("crates/expr/", CrateCategory::Expression),
    ("crates/core", CrateCategory::Core),
    ("crates/sources/", CrateCategory::Source),
    ("crates/sinks/", CrateCategory::Sink),
    ("crates/storages/", CrateCategory::Storage),
    ("crates/app", CrateCategory::App),
    ("crates/self-lint", CrateCategory::SelfLint),
];

fn classify(relative_path: &str) -> Option<CrateCategory> {
    for &(prefix, category) in CLASSIFICATION_RULES {
        if relative_path == prefix || relative_path.starts_with(prefix) {
            return Some(category);
        }
    }
    None
}

fn extract_workspace_dep_names(table: Option<&toml::Value>) -> Vec<String> {
    let Some(deps) = table.and_then(|table| table.as_table()) else {
        return Vec::new();
    };
    deps.keys()
        .filter(|key| key.starts_with("air-elt"))
        .cloned()
        .collect()
}

fn parse_crate(root: &Path, crate_path: &Path, relative_path: &str) -> Result<CrateInfo, String> {
    let toml_path = crate_path.join("Cargo.toml");
    let content = std::fs::read_to_string(&toml_path)
        .map_err(|error| format!("cannot read {}: {error}", toml_path.display()))?;
    let doc: toml::Value = toml::from_str(&content)
        .map_err(|error| format!("cannot parse {}: {error}", toml_path.display()))?;

    let name = doc
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(|name| name.as_str())
        .ok_or(format!("missing [package].name in {}", toml_path.display()))?
        .to_string();

    let dependencies = extract_workspace_dep_names(doc.get("dependencies"));
    let dev_dependencies = extract_workspace_dep_names(doc.get("dev-dependencies"));

    let has_lib = crate_path.join("src/lib.rs").exists();

    let doctest_disabled = doc
        .get("lib")
        .and_then(|lib| lib.get("doctest"))
        .and_then(|doctest| doctest.as_bool())
        .map(|doctest| !doctest)
        .unwrap_or(false);

    let autotests_disabled = doc
        .get("package")
        .and_then(|package| package.get("autotest"))
        .and_then(|autotest| autotest.as_bool())
        .map(|autotest| !autotest)
        .unwrap_or_else(|| {
            doc.get("package")
                .and_then(|package| package.get("autotests"))
                .and_then(|autotests| autotests.as_bool())
                .map(|autotests| !autotests)
                .unwrap_or(false)
        });

    let has_test_all = doc
        .get("test")
        .and_then(|test| test.as_array())
        .map(|tests| {
            tests.iter().any(|test| {
                let name_match = test
                    .get("name")
                    .and_then(|name| name.as_str())
                    .is_some_and(|name| name == "all");
                let path_match = test
                    .get("path")
                    .and_then(|path| path.as_str())
                    .is_some_and(|path| path == "tests/all.rs");
                name_match && path_match
            })
        })
        .unwrap_or(false);

    let tests_dir = root.join(relative_path).join("tests");
    let has_tests_directory = tests_dir.is_dir();

    Ok(CrateInfo {
        name,
        path: crate_path.to_path_buf(),
        relative_path: relative_path.to_string(),
        category: classify(relative_path),
        dependencies,
        dev_dependencies,
        has_lib,
        doctest_disabled,
        autotests_disabled,
        has_test_all,
        has_tests_directory,
    })
}
