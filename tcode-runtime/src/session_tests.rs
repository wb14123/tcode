use crate::session::{is_valid_session_id, validate_session_id, validate_session_path};

#[test]
fn generated_shape_session_ids_are_valid() {
    assert!(is_valid_session_id("abc123xy"));
    assert!(validate_session_id("abc123xy").is_ok());
    assert!(validate_session_path("abc123xy").is_ok());
}

#[test]
fn path_like_session_ids_are_rejected_as_root_ids() {
    for session_id in [
        "../abcde",
        "abc/1234",
        "abc1234/",
        ".abc1234",
        "ABC123XY",
        "abc123x",
        "abc123xyz",
        "subagent-foo",
    ] {
        assert!(
            !is_valid_session_id(session_id),
            "{session_id} should be invalid"
        );
        assert!(validate_session_id(session_id).is_err());
    }
}

#[test]
fn subagent_session_paths_under_valid_roots_are_valid() {
    assert!(validate_session_path("abc123xy/subagent-conv_123-456").is_ok());
    assert!(validate_session_path("abc123xy/subagent-parent/subagent-child").is_ok());
}

#[test]
fn unsafe_session_paths_are_rejected() {
    for session_id in [
        "abc123xy/../evil",
        "abc123xy/not-subagent",
        "abc123xy/subagent-",
        "abc123xy/subagent-../evil",
        "badroot/subagent-child",
    ] {
        assert!(
            validate_session_path(session_id).is_err(),
            "{session_id} should be invalid"
        );
    }
}
