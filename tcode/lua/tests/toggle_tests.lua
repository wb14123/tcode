-- Expand/collapse toggles (the `o` keymap): both the thinking indicator and
-- the tool-args preview must toggle without leaving the display buffer
-- modifiable, even across errors.

test('toggle_thinking: expand and collapse roundtrip', function()
  local b = new_buf()
  seed(b, { 'x', 'L1', 'L2' })
  local t = T.thinking_state
  t.is_thinking = true
  t.start_row = 1
  t.content_parts = { 'L1\nL2' }
  t.written = true
  T.collapse_thinking(b, ns)
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})
  local mark_id = marks[1] and marks[1][1]
  check(mark_id ~= nil, 'indicator extmark created')

  T.toggle_thinking(b, mark_id)
  local l = lines_of(b)
  check(l[2] == 'L1' and l[3] == 'L2', 'expand restores the thinking content')
  check(vim.bo[b].modifiable == false, 'buffer still non-modifiable after expand')

  T.toggle_thinking(b, mark_id)
  l = lines_of(b)
  check(l[2] == '', 'collapse back to the indicator line')
  check(vim.bo[b].modifiable == false, 'buffer still non-modifiable after collapse')
  reset_thinking()
end)

test('toggle_tool_call_args: expand and collapse roundtrip', function()
  local b = new_buf()
  T.reset_first_event()
  seed(b, { '' })
  local steps = {
    { AssistantMessageStart = {} },
    { AssistantToolCallStart = { tool_name = 'bash', tool_call_id = 'tc1', tool_call_index = 1 } },
    { AssistantToolCallArgChunk = { tool_call_index = 1, content = 'a\nb\nc\nd' } },
    { ToolMessageStart = { tool_name = 'bash', tool_call_id = 'tc1' } },
  }
  local all_ok = true
  for _, ev in ipairs(steps) do
    if not pcall(windowed_render, b, ev, false) then
      all_ok = false
    end
  end
  check(all_ok, 'render sequence (fence close + args collapse) raises no errors')

  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})
  local args_mark = marks[1] and marks[1][1]
  check(args_mark ~= nil, 'expand-hint extmark created by the args collapse')

  T.toggle_tool_call_args(b, args_mark)
  local l = lines_of(b)
  check(table.concat(l, '|'):find('|a|b|c|d|', 1, true) ~= nil, 'expand restores the full args content')
  check(vim.bo[b].modifiable == false, 'buffer still non-modifiable after expand')

  T.toggle_tool_call_args(b, args_mark)
  l = lines_of(b)
  check(table.concat(l, '|'):find('|a|b|c|d|', 1, true) == nil, 'collapse back to the preview line')
  check(vim.bo[b].modifiable == false, 'buffer still non-modifiable after collapse')
end)
