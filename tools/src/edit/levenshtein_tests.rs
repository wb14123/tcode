use crate::edit::levenshtein::{distance, similarity};

#[test]
fn test_distance_identical() {
    assert_eq!(distance("hello", "hello"), 0);
}

#[test]
fn test_distance_empty() {
    assert_eq!(distance("", "hello"), 5);
    assert_eq!(distance("hello", ""), 5);
    assert_eq!(distance("", ""), 0);
}

#[test]
fn test_distance_one_edit() {
    assert_eq!(distance("hello", "hallo"), 1);
    assert_eq!(distance("cat", "car"), 1);
}

#[test]
fn test_distance_multiple_edits() {
    assert_eq!(distance("kitten", "sitting"), 3);
}

#[test]
fn test_similarity_identical() {
    assert!((similarity("hello", "hello") - 1.0).abs() < f64::EPSILON);
}

#[test]
fn test_similarity_empty() {
    assert!((similarity("", "") - 1.0).abs() < f64::EPSILON);
}

#[test]
fn test_similarity_completely_different() {
    assert!(similarity("abc", "xyz") < 0.1);
}

#[test]
fn test_similarity_partial() {
    let sim = similarity("hello", "hallo");
    assert!(sim > 0.7);
    assert!(sim < 1.0);
}
