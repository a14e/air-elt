# Goal

This task extends the capabilities of working with fields in mapping.

# General description

1. Add the ability to use an asterisk in mapping
```
mapping = ["*"]
```

How does it work?

If the sink has a schema, take the sink schema; if the sink has none, take the source schema; if neither has one — drop validation.

Also important for the asterisk: it does not overwrite types declared before or after it, i.e. it does not participate in duplicate checks.

In addition, we require that the names on the input and output sides match.

2. Adding a short-form notation

Now, instead of
```
mapping = [{from="created_at", to="created_at"}]
```
you can write
```
mapping = ["created_at"]
```
or
```
mapping = ["created_at:created_at"]
```
i.e. we add a short-form notation.

And of course
```
mapping = ["*:*"]
```
will work too — same as `"*"`.

The form `["field:*"]` is forbidden because it is ambiguous.

3. Add JSON auto-packing
```
mapping = [
   "*:body"
]
```
which is equivalent to
```
mapping = [
   {from = "*", to = "body"}
]
```

In this case all fields of the source structure will be copied into a JSON object. This is convenient when working with various unstructured fields.

# Other requirements
1. Before starting the task, translate the entire description of this assignment into English (in this file).
2. Do not forget about tests.
3. Run validator agents after execution.
4. Use the task-flow skill before drafting the plan (and follow it during execution as well).
5. After the first version of the plan, run it through a dedicated validator agent.
6. Separately validate the plan with style, business-requirements, db-protocols, and resource-leak agents (as many agents as possible).
7. Before all of that, also run the main agents to collect a list of implementation questions.

That is, multi-level verification of requirements is expected before implementation. Run the requirement-validator agents after you have collected the necessary information — give them a digest and let them validate.

After collecting requirements, draft the plan and then iterate on it.
