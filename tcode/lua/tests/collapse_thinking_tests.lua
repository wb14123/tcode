-- collapse_thinking: collapses an expanded thinking block to a one-line
-- indicator. The collapse range is the block's OWN region (model-driven).
-- Content below a streaming block (subagent labels, tool output, system
-- messages) is never swallowed because the reducer collapses the expanded
-- block before any new element opens. A collapsed block is ONE real buffer
-- row, resolved through the integer row map (no extmarks).

local function row_of(m, el)
  return T.get_renderer_state(m).rows[el.id]
end

local function first_of_type(m, type_)
  for _, el in ipairs(m.elements) do
    if el.type == type_ then return el end
  end
  return nil
end

test('collapse live: only thinking rows collapse, content below preserved', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { 'line0', '' })
  -- The block streams onto the rows after the seeded blank.
  windowed_render(b, { AssistantThinkingChunk = { content = 'THINK-A\nTHINK-B' } }, false)
  -- The reducer collapses the expanded thinking BEFORE adding the system message
  -- (the structural equivalent of the old content-derived collapse range).
  windowed_render(b, { SystemMessage = { level = 'Info', message = 'BELOW' } }, false)
  local l = lines_of(b)
  check(l[1] == 'line0', 'row above intact')
  check(l[2] == '', 'seeded blank preserved above the block')
  check(l[3] == '► [Thinking... press o to expand]', 'the two thinking rows collapse to one real row')
  check(l[4] == '► SYSTEM [Info]' and l[5] == 'BELOW', 'content below preserved')
  local block = first_of_type(m, 'thinking_block')
  check(block ~= nil and block.state == 'collapsed', 'model block collapsed')
  local r = row_of(m, block)
  check(r.start_row == 2 and r.height == 1, 'row map: collapsed block at [2, 3)')
  local el, off = T.element_at_row_full(m, b, 2)
  check(el == block and off == 0 and T.action_at(block) == 'thinking',
    'the collapsed row resolves to (block, 0) and toggles thinking')
  check(vim.bo[b].modifiable == false, 'buffer left non-modifiable')
end)

test('collapse bulk: content materialized in the full projection then collapsed', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { '' })
  -- The bulk initial load writes EVERYTHING in one full projection: the bulk
  -- thinking content is materialized immediately, not deferred, and the new
  -- block is expanded by default (chrome row + content).
  windowed_render(b, { AssistantThinkingChunk = { content = 'BULK THINKING\nmore' } }, true)
  local block = first_of_type(m, 'thinking_block')
  local l = lines_of(b)
  check(l[1] == '► [Thinking... press o to collapse]' and l[2] == 'BULK THINKING' and l[3] == 'more',
    'bulk load materialized the chrome row + thinking content in one shot')
  local r = row_of(m, block)
  check(r.start_row == 0 and r.height == 3, 'row map: expanded block at [0, 3)')
  T.collapse_thinking(m, block, b, ns)
  l = lines_of(b)
  check(l[1] == '► [Thinking... press o to expand]', 'collapse renders one real row')
  check(block ~= nil and block.state == 'collapsed', 'model block collapsed')
  check(row_of(m, block).height == 1, 'collapsed height is 1')
  check(vim.bo[b].modifiable == false, 'buffer left non-modifiable')
end)

test('collapse is a no-op when no thinking is active', function()
  local m = T.reset_model()
  local b = new_buf()
  seed(b, { 'x', 'y' })
  T.collapse_thinking(m, nil, b, ns)
  local l = lines_of(b)
  check(l[1] == 'x' and l[2] == 'y', 'buffer unchanged')
  check(vim.bo[b].modifiable == false, 'buffer left non-modifiable')
end)
