# Mongo support


What are we doing?
We are implementing a sink and a source storage.

We follow the same approach as what currently exists for MySQL and Postgresql.

Add it to CI.

For accessing fields, add dot notation as the field description.

After completing the tasks, add a new validation level. Call it sampling-validation.

Add an optional block to the flow
called validation, where you can set true/false for sampling validation and other validations.
Implement sampling validation for all current databases. By default it is disabled for all of them except Mongo. There you can also choose the sample size
and enable/disable sampling-based validation.
Moreover, it can be described in two ways:
```yml
validation:
  sampling: true
```
```yml
validation:
  sampling: 
    enabled: true
    size: 100 # default
```
Add these configs to the skill.

The point of sampling validation: download a batch of data — by default 100 items. From the fields, infer types and validate
whether we will be able to assemble the response or not. (We check type compatibility and type nullability.)

Add sampling validation separately for the current databases as well.
Let's also add parallelization at the validation layer? The level of parallelism is by the number of sources, i.e. for each source
we create one async process that validates the data against the validations (currently the validation runs sequentially).
And print to stdout that we are running validation in n threads.



# Miscellaneous
1. run validator agents at the end
2. don't forget to add Mongo to CI
3. before starting the task, translate this task into English
