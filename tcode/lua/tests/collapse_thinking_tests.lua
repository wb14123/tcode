-- collapse_thinking: collapses an open thinking block to a one-line indicator.
-- The collapse range is the block's OWN region (model-driven). Content below
-- an unterminated block (subagent labels, tool output, system messages in
-- interrupted/attached sessions) is never swallowed because the reducer
-- collapses the open block before any new element opens.

test('collapse live: only thinking rows collapse, content below preserved', function()
  local b = new_buf()
  T.reset_model()
  seed(b, { 'line0', '' })
  -- The block streams onto the last seeded row.
  windowed_render(b, { AssistantThinkingChunk = { content = 'THINK-A\nTHINK-B' } }, false)
  -- The reducer collapses the open thinking BEFORE adding the system message
  -- (the structural equivalent of the old content-derived collapse range).
  windowed_render(b, { SystemMessage = { level = 'Info', message = 'BELOW' } }, false)
  local l = lines_of(b)
  check(l[1] == 'line0', 'row above intact')
  check(l[2] == '' and l[3] == '' and l[4] == '', 'indicator + spacer rows replace the thinking rows')
  check(l[5] == '► SYSTEM' and l[6] == 'BELOW', 'content below preserved')
  local block = nil
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' then block = el end
  end
  check(block ~= nil and block.state == 'collapsed', 'model block collapsed')
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})
  check(#marks >= 1 and marks[1][2] == 1, 'indicator extmark placed at the thinking start row')
  check(vim.bo[b].modifiable == false, 'buffer left non-modifiable')
end)

test('collapse bulk: content not written then materialized', function()
  local b = new_buf()
  T.reset_model()
  seed(b, { 'line0', '' })
  windowed_render(b, { AssistantThinkingChunk = { content = 'BULK THINKING\nmore' } }, true)
  T.collapse_thinking(b, ns)
  local l = lines_of(b)
  check(l[1] == 'line0', 'row above intact')
  check(l[2] == '' and l[3] == '' and l[4] == '', 'anchor line replaced by indicator + spacer rows')
  local block = nil
  for _, el in ipairs(T.model.elements) do
    if el.type == 'thinking_block' then block = el end
  end
  check(block ~= nil and block.state == 'collapsed', 'model block collapsed')
  check(vim.bo[b].modifiable == false, 'buffer left non-modifiable')
end)

test('collapse is a no-op when no thinking is active', function()
  local b = new_buf()
  T.reset_model()
  seed(b, { 'x', 'y' })
  T.collapse_thinking(b, ns)
  local l = lines_of(b)
  check(l[1] == 'x' and l[2] == 'y', 'buffer unchanged')
  check(vim.bo[b].modifiable == false, 'buffer left non-modifiable')
end)
