pub mod arithmetic;
pub mod bitwise;
pub mod bytes;
pub mod cast;
pub mod comparison;
pub mod conditional;
pub mod crypto;
pub mod datetime;
pub mod encoding;
pub mod env;
pub mod json;
pub mod logical;
pub mod math;
pub mod misc;
pub mod object;
pub mod random;
pub mod string;

pub fn register_builtins(registry: &mut crate::FunctionRegistry) {
    arithmetic::register(registry);
    bitwise::register(registry);
    bytes::register(registry);
    cast::register(registry);
    comparison::register(registry);
    conditional::register(registry);
    crypto::register(registry);
    datetime::register(registry);
    encoding::register(registry);
    env::register(registry);
    json::register(registry);
    logical::register(registry);
    math::register(registry);
    misc::register(registry);
    object::register(registry);
    random::register(registry);
    string::register(registry);
}
