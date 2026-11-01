pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
mod text;
pub use text::*;
mod model;
pub use model::*;
mod personal;
pub use personal::*;
mod search;
pub use search::*;

#[cfg(test)]
mod tests {
    #[test]
    fn test_normalize_unicode() {
        assert_eq!(super::normalize("Е\u{308}Ж"), "ёж");
    }
}
