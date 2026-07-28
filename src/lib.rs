#![forbid(unsafe_code)]

/// Builds the command-line greeting.
#[must_use]
pub fn greeting(project_name: &str) -> String {
    format!("Hello from {project_name}!")
}

#[cfg(test)]
mod tests {
    use super::greeting;

    #[test]
    fn builds_project_greeting() {
        assert_eq!(greeting("rust-template"), "Hello from rust-template!");
    }
}
