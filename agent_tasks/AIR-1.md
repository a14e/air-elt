# Task


Building the basic version of the project.

In accordance with README.md.


Steps:
1. Basic implementation of the core (with configs, traits, etc.)
2. Implement a PostgreSQL sink and source, plus config validation there with the main checks
3. Implement e2e tests with PostgreSQL
4. Implement migrations using raw SQL and sqlx
5. Implement the basic application startup
6. Implement the main data types (no int conversions or transformations yet — just a basic implementation)
7. Implement the basic standard types and the conversion matrix to/from these types, with validation against them




# Skill and README
Move what's currently in the README into a separate skill. In the README keep the basic info about the project aimed at a human reader.

Also add a basic skill where I'll be adding descriptions of the general rules for working with Rust.

For now add just a few points there (I'll add more later):
* follow best practices
* readability is a priority over efficiency for us. Better to write simple, understandable code with cloning than complex references
* nevertheless, prefer transferring ownership over cloning
* pass `String` by reference as `&str`


# Misc
* At the end, run validator agents and clippy
* Let's also add mimalloc to the application up front
