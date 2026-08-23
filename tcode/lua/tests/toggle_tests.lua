-- Expand/collapse toggles (the `o` keymap): the thinking indicator must
-- toggle without leaving the display buffer modifiable, even across errors.
-- The toggle mutates the model through the reducer and re-renders the
-- element's region from the integer row map. (Tool/subagent toggles are gone:
-- `o` opens the detail view from every tool row; covered in
-- renderer_tests.lua / regression_tests.lua.)

local function row_of(m, el)
  return T.get_renderer_state(m).rows[el.id]
end

local function last_thinking(m)
  local el = nil
  for _, e in ipairs(m.elements) do
    if e.type == 'thinking_block' then el = e end
  end
  return el
end

test('toggle_thinking: expand and collapse roundtrip', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'L1\nL2' } }, false)
  local block = last_thinking(m)
  T.collapse_thinking(m, block, b, ns)
  check(block.state == 'collapsed', 'block collapsed first')

  T.toggle_thinking(m, block, b, ns)
  local l = lines_of(b)
  check(l[2] == '► [Thinking... press o to collapse]' and l[3] == 'L1' and l[4] == 'L2',
    'expand restores the hint + full thinking content')
  check(block.state == 'expanded', 'block expanded')
  local r = row_of(m, block)
  check(r.start_row == 1 and r.height == 3, 'row map: expanded block at [1, 4)')
  check(vim.bo[b].modifiable == false, 'buffer still non-modifiable after expand')

  T.toggle_thinking(m, block, b, ns)
  l = lines_of(b)
  check(l[2] == '► [Thinking... press o to expand]', 'collapse back to the single indicator row')
  check(block.state == 'collapsed', 'block collapsed again')
  check(row_of(m, block).height == 1, 'collapsed block height 1')
  local el, off = T.element_at_row_full(m, b, 1)
  check(el == block and off == 0 and T.action_at(block, 0) == 'thinking',
    'the indicator row resolves to (block, 0) and toggles thinking')
  check(vim.bo[b].modifiable == false, 'buffer still non-modifiable after collapse')
end)
