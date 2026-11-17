//! Minimal library for nextest archive timing pilots.

/// Adds two integers.
///
/// ```
/// assert_eq!(nextest_pilot::add(1, 1), 2);
/// ```
pub fn add(left: i32, right: i32) -> i32 {
    left + right
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_plus_one_is_two() {
        assert_eq!(1 + 1, 2);
    }
}