use crate::registry::FunctionRegistry;

pub fn register_builtins(registry: &mut FunctionRegistry) {
    super::arithmetic::register(registry);
    super::bitwise::register(registry);
    super::bytes::register(registry);
    super::cast::register(registry);
    super::comparison::register(registry);
    super::conditional::register(registry);
    super::crypto::register(registry);
    super::datetime::register(registry);
    super::encoding::register(registry);
    super::env::register(registry);
    super::json::register(registry);
    super::logical::register(registry);
    super::math::register(registry);
    super::misc::register(registry);
    super::object::register(registry);
    super::random::register(registry);
    super::regex::register(registry);
    super::string::register(registry);
}
