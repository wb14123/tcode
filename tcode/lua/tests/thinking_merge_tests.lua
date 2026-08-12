-- Merge behavior for consecutive thinking runs with nothing visible between
-- them. One continuous reasoning stream (DeepSeek bursts via OpenRouter,
-- Claude back-to-back thinking blocks) can be split into multiple collapsed
-- entries when the 500ms settle flush fires during a pause; the next run must
-- merge into the previous entry instead of creating a second one. When any
-- real content (text, tool labels, subagent sections, an expanded entry) sits
-- between the runs, they must stay separate.

local function entry_count(b)
  local ids = {}
  for _, m in ipairs(vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})) do
    ids[m[1]] = true
  end
  local n = 0
  for id, _ in pairs(T.thinking_entries) do
    if ids[id] then n = n + 1 end
  end
  return n
end

local function entry_contents(b)
  local ids = {}
  for _, m in ipairs(vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})) do
    ids[m[1]] = true
  end
  local contents = {}
  for id, entry in pairs(T.thinking_entries) do
    if ids[id] then
      contents[#contents + 1] = entry.content
    end
  end
  table.sort(contents)
  return contents
end

test('merge: runs split by a pause collapse into a single entry', function()
  local b = new_buf()
  seed(b, { 'label', '' })
  -- Run 1 streams live, then the settle flush collapses it.
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1\nA2' } }, false)
  check(T.thinking_state.is_thinking == true, 'run 1 open')
  T.collapse_thinking(b, ns)
  check(T.thinking_state.is_thinking == false, 'run 1 collapsed by the flush')
  -- Run 2 arrives after the pause: only blank spacer rows between -> merge.
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = '\nB2' } }, false)
  check(T.thinking_state.is_thinking == true, 'run 2 merged and streaming')
  local l = lines_of(b)
  check(l[1] == 'label', 'label row intact')
  check(l[2] == 'B1' and l[3] == 'B2', 'run 2 content streams visibly from the merged anchor')
  -- Final collapse yields ONE entry with the combined text.
  T.collapse_thinking(b, ns)
  check(entry_count(b) == 1, 'single thinking entry after the merged collapse')
  local contents = entry_contents(b)
  check(contents[1] == 'A1\nA2B1\nB2', 'entry holds both runs in order')
  l = lines_of(b)
  check(l[1] == 'label' and l[2] == '' and l[3] == '' and l[4] == '', 'collapsed to one indicator + spacers')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
  reset_thinking()
end)

test('merge: visible text between runs keeps separate entries', function()
  local b = new_buf()
  T.reset_first_event()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1' } }, false)
  T.collapse_thinking(b, ns)
  -- A real text chunk sits between the thinking runs: no merge.
  windowed_render(b, { AssistantMessageChunk = { content = 'visible text\n' } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  check(T.thinking_state.is_thinking == true, 'second run open as its own entry')
  T.collapse_thinking(b, ns)
  check(entry_count(b) == 2, 'two separate entries kept')
  local contents = entry_contents(b)
  check(contents[1] == 'A1' and contents[2] == 'B1', 'entries hold their own content')
  local l = lines_of(b)
  check(table.concat(l, '|'):find('visible text', 1, true) ~= nil, 'text preserved between the two indicators')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
  reset_thinking()
end)

test('merge: a tool call between runs keeps separate entries', function()
  local b = new_buf()
  T.reset_first_event()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1' } }, false)
  T.collapse_thinking(b, ns)
  -- A tool call starts between the thinking runs: no merge.
  windowed_render(b, { AssistantToolCallStart = { tool_name = 'bash', tool_call_id = 'tc1', tool_call_index = 1 } }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  check(T.thinking_state.is_thinking == true, 'second run open as its own entry')
  T.collapse_thinking(b, ns)
  check(entry_count(b) == 2, 'two separate entries kept (one per thinking phase)')
  local l = lines_of(b)
  check(table.concat(l, '|'):find('TOOL', 1, true) ~= nil, 'tool label preserved between the indicators')
  reset_thinking()
end)

test('merge: an expanded previous entry is never swallowed', function()
  local b = new_buf()
  T.reset_first_event()
  seed(b, { '' })
  windowed_render(b, { AssistantMessageStart = {} }, false)
  windowed_render(b, { AssistantThinkingChunk = { content = 'A1' } }, false)
  T.collapse_thinking(b, ns)
  local marks = vim.api.nvim_buf_get_extmarks(b, thinking_ns_id, 0, -1, {})
  local mark_id = marks[1] and marks[1][1]
  check(mark_id ~= nil, 'indicator extmark present')
  T.toggle_thinking(b, mark_id)  -- user expanded the entry to read it
  windowed_render(b, { AssistantThinkingChunk = { content = 'B1' } }, false)
  check(T.thinking_state.is_thinking == true, 'new run opened without merging')
  T.collapse_thinking(b, ns)
  check(entry_count(b) == 2, 'two separate entries kept')
  local l = lines_of(b)
  check(table.concat(l, '|'):find('A1', 1, true) ~= nil, 'expanded content still visible in the buffer')
  local contents = entry_contents(b)
  check(contents[1] == 'A1' and contents[2] == 'B1', 'both entries hold their own content')
  check(vim.bo[b].modifiable == false, 'buffer non-modifiable')
  reset_thinking()
end)

test('merge: bulk runs separated by a whitespace text chunk merge', function()
  local b = new_buf()
  T.reset_first_event()
  seed(b, { '' })
  -- Bulk load: the whitespace text chunk between the runs triggers a collapse,
  -- and the deferred second run must merge back into the same entry.
  windowed_render(b, { AssistantMessageStart = {} }, true)
  windowed_render(b, { AssistantThinkingChunk = { content = 'bulk one' } }, true)
  windowed_render(b, { AssistantMessageChunk = { content = '\n' } }, true)
  windowed_render(b, { AssistantThinkingChunk = { content = 'bulk two' } }, true)
  check(T.thinking_state.is_thinking == true, 'bulk run open after the whitespace collapse')
  T.collapse_thinking(b, ns)
  check(entry_count(b) == 1, 'bulk runs merged into a single entry')
  local contents = entry_contents(b)
  check(contents[1] == 'bulk onebulk two', 'merged entry holds both bulk runs')
  reset_thinking()
end)
