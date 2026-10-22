use std::sync::Arc;

use anyhow::Result;
use llm_rs::fetch_budget::{DEFAULT_WEB_FETCH_CHAR_BUDGET, WebFetchBudget};
use llm_rs::media::ContentPart;
use llm_rs::permission::ScopedPermissionManager;
use llm_rs::tool::{CancellationToken, ToolContext};
use tokio_stream::StreamExt;

use super::{
    BudgetSummary, TruncationReason, apply_truncation, blocked_notice, budgeted_max_length,
    charge_text, format_result, media_budget_line, result_reason, web_fetch_tool,
};

/// A budget node with no parent and no persistence.
fn node(remaining: u32) -> Arc<WebFetchBudget> {
    WebFetchBudget::new(remaining, "test".to_string(), None, None)
}

/// A `ToolContext` whose web_fetch budget is `budget`.
fn ctx(budget: Option<Arc<WebFetchBudget>>) -> ToolContext {
    ToolContext {
        cancel_token: CancellationToken::new(),
        permission: ScopedPermissionManager::always_allow("test"),
        container_config: None,
        session_dir: None,
        supports_media: false,
        llm: None,
        model: None,
        media_sink: None,
        web_fetch_budget: budget,
    }
}

fn text_of(part: &ContentPart) -> &str {
    match part {
        ContentPart::Text(text) => text,
        other => panic!("expected a text part, got {other:?}"),
    }
}

#[test]
fn normal_result_carries_budget_line() {
    let got = format_result(
        "hello",
        5,
        TruncationReason::None,
        0,
        Some(BudgetSummary {
            remaining: 388_000,
            used: 12_000,
        }),
    );
    let expected = "[web_fetch] Budget (web_fetch only): 388000 chars remaining for this subagent tree (this call used 12000).\n\
                    [web_fetch] Showing full content (5 chars).\n\
                    hello";
    assert_eq!(got, expected);
}

#[test]
fn budget_limited_line_reports_granted_chars_and_offset() {
    let got = format_result(
        "abcde",
        10,
        TruncationReason::BudgetLimited {
            shown: 1500,
            skip: 0,
        },
        0,
        Some(BudgetSummary {
            remaining: 0,
            used: 1500,
        }),
    );
    let expected = "[web_fetch] Budget (web_fetch only): 0 chars remaining for this subagent tree (this call used 1500).\n\
                    [web_fetch] Output limited by the remaining web_fetch budget: showing 1500 chars starting at 0. \
                    The next fetch will be blocked; use the other tools normally and report what you have unless \
                    more of this page is essential.\n\
                    abcde";
    assert_eq!(got, expected);
}

#[test]
fn max_length_truncation_with_budget_advises_skip_chars() {
    let content = "x".repeat(20_000);
    let got = format_result(
        &content,
        90_000,
        TruncationReason::MaxLength,
        20_000,
        Some(BudgetSummary {
            remaining: 360_000,
            used: 20_000,
        }),
    );
    let expected = format!(
        "[web_fetch] Budget (web_fetch only): 360000 chars remaining for this subagent tree (this call used 20000).\n\
         [web_fetch] Content truncated. Showing chars 20000-40000 of 90000.\n\
         There are 50000 chars remaining. To read the rest, call web_fetch again with skip_chars=40000; \
         further fetches cost the same tree budget.\n\
         {content}"
    );
    assert_eq!(got, expected);
}

#[test]
fn max_length_truncation_without_budget_keeps_legacy_wording() {
    let content = "x".repeat(20_000);
    let got = format_result(&content, 90_000, TruncationReason::MaxLength, 20_000, None);
    let expected = format!(
        "[web_fetch] Content truncated. Showing chars 20000-40000 of 90000.\n\
         There are 50000 chars remaining. To read the full page, create separate sub agents \
         each fetching a different portion using skip_chars (e.g. skip_chars=40000). \
         Each sub agent should summarize its own chunk to maintain a smaller context window.\n\
         {content}"
    );
    assert_eq!(got, expected);
    assert!(!got.contains("[web_fetch] Budget"));
}

#[test]
fn no_budget_full_result_has_no_budget_line() {
    let got = format_result("hello", 5, TruncationReason::None, 0, None);
    assert_eq!(got, "[web_fetch] Showing full content (5 chars).\nhello");
}

#[test]
fn exhausted_notice_text() {
    let expected = "[web_fetch] Blocked: the web_fetch character budget for this subagent tree is exhausted (0 chars remaining). Nothing was sent.\n\
                    Do not retry web_fetch. Report what you already have; this budget only limits web_fetch, \
                    so keep using the other tools as needed.";
    assert_eq!(blocked_notice(), expected);
}

#[test]
fn media_budget_line_says_not_charged() {
    let expected = "[web_fetch] Budget (web_fetch only): 388000 chars remaining for this subagent tree \
                    (images and PDFs are not charged).";
    assert_eq!(media_budget_line(388_000), expected);
}

#[test]
fn budget_limited_wording_only_when_content_was_cut() {
    let roomy = node(100);
    let short_page = charge_text(roomy.as_ref(), "12345");
    assert_eq!(short_page.granted, 5);
    assert_eq!(
        result_reason(20_000, 100, 0, false, &short_page),
        TruncationReason::None
    );

    let tiny = node(5);
    let cut = charge_text(tiny.as_ref(), "abcdefghij");
    assert_eq!(
        result_reason(20_000, 5, 0, false, &cut),
        TruncationReason::BudgetLimited { shown: 5, skip: 0 }
    );
}

#[test]
fn budget_limited_offset_follows_skip_chars() {
    let tiny = node(5);
    let cut = charge_text(tiny.as_ref(), "abcdefghij");
    let reason = result_reason(20_000, 5, 3, false, &cut);
    assert_eq!(
        reason,
        TruncationReason::BudgetLimited { shown: 5, skip: 3 }
    );
    let got = format_result(&cut.text, 10, reason, 3, Some(cut.summary()));
    assert!(got.contains("showing 5 chars starting at 3."), "{got}");
}

#[test]
fn clamp_that_cuts_the_page_is_budget_limited() {
    let exact = node(1500);
    let text = "y".repeat(1500);
    let charged = charge_text(exact.as_ref(), &text);
    assert_eq!(charged.granted, charged.cost);
    assert_eq!(
        result_reason(20_000, 1500, 0, true, &charged),
        TruncationReason::BudgetLimited {
            shown: 1500,
            skip: 0
        }
    );
}

#[test]
fn page_that_ends_at_the_clamp_without_skip_is_not_budget_limited() {
    let exact = node(1500);
    let text = "y".repeat(1500);
    let charged = charge_text(exact.as_ref(), &text);
    assert_eq!(charged.granted, charged.cost);
    assert_eq!(
        result_reason(20_000, 1500, 0, false, &charged),
        TruncationReason::None,
        "the whole page was sent, so the budget did not limit the output"
    );
}

#[test]
fn grant_truncates_the_sent_text() {
    let tiny = node(5);
    let out = charge_text(tiny.as_ref(), "abcdefghij");
    assert_eq!(out.cost, 10);
    assert_eq!(out.granted, 5);
    assert_eq!(out.text, "abcde");
    assert_eq!(out.remaining, 0);
    assert!(!out.is_blocked());
}

#[test]
fn budget_line_reports_the_granted_amount_when_the_charge_is_cut() {
    let tiny = node(5);
    let out = charge_text(tiny.as_ref(), "abcdefghij");
    let reason = result_reason(20_000, 5, 0, false, &out);
    let got = format_result(&out.text, 10, reason, 0, Some(out.summary()));
    assert!(got.contains("(this call used 5)."), "{got}");
}

#[test]
fn empty_page_is_a_normal_result() {
    let roomy = node(10);
    let out = charge_text(roomy.as_ref(), "");
    assert_eq!(out.cost, 0);
    assert_eq!(out.granted, 0);
    assert_eq!(out.text, "");
    assert!(!out.is_blocked());
}

#[test]
fn lost_race_with_non_empty_page_is_blocked() {
    let empty = node(0);
    let out = charge_text(empty.as_ref(), "abc");
    assert_eq!(out.cost, 3);
    assert_eq!(out.granted, 0);
    assert!(out.is_blocked());
}

#[test]
fn charge_counts_chars_not_bytes() {
    let roomy = node(10);
    let out = charge_text(roomy.as_ref(), "日本語");
    assert_eq!("日本語".len(), 9);
    assert_eq!(out.cost, 3);
    assert_eq!(out.granted, 3);
    assert_eq!(out.remaining, 7);
}

#[test]
fn budgeted_max_length_uses_the_remaining_budget() {
    let tight = node(1500);
    assert_eq!(budgeted_max_length(20_000, tight.as_ref()), Some(1500));
    assert_eq!(budgeted_max_length(500, tight.as_ref()), Some(500));

    let exhausted = node(0);
    assert_eq!(budgeted_max_length(20_000, exhausted.as_ref()), None);
}

#[test]
fn zero_max_length_is_not_reported_as_exhaustion() {
    let roomy = node(1000);

    assert_eq!(budgeted_max_length(0, roomy.as_ref()), Some(0));
    assert_eq!(roomy.get(), 1000, "clamping must not spend anything");
}

#[test]
fn apply_truncation_behaves_as_before() {
    let (content, total, truncated) = apply_truncation("abcdefghij", 4, 2);
    assert_eq!(content, "cdef");
    assert_eq!(total, 10);
    assert!(truncated);

    let (content, total, truncated) = apply_truncation("abc", 5, 0);
    assert_eq!(content, "abc");
    assert_eq!(total, 3);
    assert!(!truncated);

    // `total_length` is reported in bytes, as it always has been.
    let (content, total, truncated) = apply_truncation("日本語abc", 2, 0);
    assert_eq!(content, "日本");
    assert_eq!(total, 12);
    assert!(truncated);

    let (content, total, truncated) = apply_truncation("abc", 5, 10);
    assert_eq!(content, "");
    assert_eq!(total, 3);
    assert!(truncated);
}

#[tokio::test]
async fn exhausted_budget_blocks_before_any_network_access() -> Result<()> {
    let tool = web_fetch_tool();
    let mut stream = tool.execute(
        ctx(Some(node(0))),
        r#"{"url": "https://example.com/page"}"#.to_string(),
    );
    let first = stream.next().await.expect("stream produced no item");
    assert_eq!(text_of(&first), blocked_notice());
    Ok(())
}

#[tokio::test]
async fn exhausted_budget_blocks_before_the_media_probe() -> Result<()> {
    let tool = web_fetch_tool();
    let mut stream = tool.execute(
        ctx(Some(node(0))),
        r#"{"url": "https://example.com/image.png"}"#.to_string(),
    );
    let first = stream.next().await.expect("stream produced no item");
    assert_eq!(text_of(&first), blocked_notice());
    assert!(stream.next().await.is_none());
    Ok(())
}

#[test]
fn description_documents_the_default_budget() {
    let description = web_fetch_tool().description;

    assert!(
        description.contains(&DEFAULT_WEB_FETCH_CHAR_BUDGET.to_string()),
        "the web_fetch description must state the default budget: {description}"
    );
}
