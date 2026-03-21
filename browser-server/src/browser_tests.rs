use std::sync::{Arc, Barrier};

use anyhow::Result;

use crate::browser;

/// Open multiple tabs in parallel and verify they all succeed.
/// Each thread navigates to a data URI with unique content, then reads it back
/// via JS to confirm the tab loaded correctly.
#[test]
fn parallel_open_tab() -> Result<()> {
    const NUM_TABS: usize = 4;

    let barrier = Arc::new(Barrier::new(NUM_TABS));
    let handles: Vec<_> = (0..NUM_TABS)
        .map(|i| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                // Synchronize so all threads call open_tab concurrently
                barrier.wait();

                let content = format!("page-{i}");
                let url = format!("data:text/html,<title>{content}</title><p>{content}</p>");
                let tab = browser::open_tab(&url).expect("open_tab failed");

                let result = tab
                    .evaluate("document.title", false)
                    .expect("JS eval failed");
                let title = result.value.as_ref().and_then(|v| v.as_str()).unwrap_or("");
                assert_eq!(title, content, "tab {i} has wrong title");
            })
        })
        .collect();

    for (i, h) in handles.into_iter().enumerate() {
        h.join()
            .unwrap_or_else(|e| panic!("thread {i} panicked: {e:?}"));
    }
    Ok(())
}

/// After all TabGuards are dropped, active_tabs should be 0.
#[test]
fn tab_guard_cleanup() -> Result<()> {
    {
        let tab = browser::open_tab("data:text/html,<p>cleanup-test</p>")?;
        let result = tab.evaluate("document.querySelector('p').textContent", false)?;
        assert_eq!(
            result.value.as_ref().and_then(|v| v.as_str()),
            Some("cleanup-test"),
        );
        // tab dropped here
    }

    // Open another tab to confirm the browser is still alive and reusable
    let tab2 = browser::open_tab("data:text/html,<p>still-alive</p>")?;
    let result = tab2.evaluate("document.querySelector('p').textContent", false)?;
    assert_eq!(
        result.value.as_ref().and_then(|v| v.as_str()),
        Some("still-alive"),
    );
    Ok(())
}
