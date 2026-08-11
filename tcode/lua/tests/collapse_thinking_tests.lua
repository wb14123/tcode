-- collapse_thinking: collapses an open thinking block to a one-line indicator.
-- The collapse range must be computed from the thinking block's own content,
-- NOT the buffer end, so content appended below an unterminated block
-- (subagent labels, tool output, system messages in interrupted/attached
-- sessions) is never swallowed.

test('collapse live: only thinking rows collapse, content below preserved', function()
  local b = new_buf()
  seed(b, { 'line0', 'THINK-A', 'THINK-B', 'SUBAGENT LABEL', 'subagent content' })
  local t = T.thinking_state
  t.is_thinking = true
  t.start_row = 1
  t.content_parts = { 'THINK-A\nTHINK-B' }
  t.last_highlighted_row = 0
  t.written = true
  T.collapse_thinking(b, ns)
  local l = lines_of(b)
  check(l[1] == 'line0', 'row above intact')
  check(l[2] == '' and l[3] == '' and l[4] == '', 'indicator + spacer rows replace the thinking rows')
  check(l[5] == 'SUBAGENT LABEL' and l[6] == 'subagent content', 'content below preserved')
  check(t.is_thinking == false, 'state reset: is_thinking false')
  check(#t.content_parts == 0, 'state reset: content parts cleared')
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})
  check(#marks >= 1 and marks[1][2] == 1, 'indicator extmark placed at the thinking start row')
  check(vim.bo[b].modifiable == false, 'buffer left non-modifiable')
  reset_thinking()
end)

test('collapse bulk: only the anchor line collapses (content not written)', function()
  local b = new_buf()
  seed(b, { 'line0', '', 'SUBAGENT LABEL', 'subagent content' })
  local t = T.thinking_state
  t.is_thinking = true
  t.start_row = 1
  t.content_parts = { 'BULK THINKING\nmore' }
  t.written = false
  T.collapse_thinking(b, ns)
  local l = lines_of(b)
  check(l[1] == 'line0', 'row above intact')
  check(l[2] == '' and l[3] == '' and l[4] == '', 'anchor line replaced by indicator + spacer rows')
  check(l[5] == 'SUBAGENT LABEL' and l[6] == 'subagent content', 'content below preserved')
  check(vim.bo[b].modifiable == false, 'buffer left non-modifiable')
  reset_thinking()
end)

test('collapse is a no-op when no thinking is active', function()
  local b = new_buf()
  seed(b, { 'x', 'y' })
  reset_thinking()
  T.collapse_thinking(b, ns)
  local l = lines_of(b)
  check(l[1] == 'x' and l[2] == 'y', 'buffer unchanged')
end)
