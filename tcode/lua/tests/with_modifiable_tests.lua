-- with_modifiable: the modifiable-window helper that all scheduled display
-- writes run inside. It must restore the previous modifiable state even when
-- the wrapped function errors, and must be safe to nest (the JSONL batch
-- render opens one window; render helpers run inside it).

test('with_modifiable: modifiable is true inside the window', function()
  local b = new_buf()
  local seen
  T.with_modifiable(b, function()
    seen = vim.bo[b].modifiable
  end)
  check(seen == true, 'modifiable is true inside the window')
  check(vim.bo[b].modifiable == false, 'restored to false after success')
end)

test('with_modifiable: restores and re-raises on error', function()
  local b = new_buf()
  local ok = pcall(function()
    T.with_modifiable(b, function()
      error('boom')
    end)
  end)
  check(not ok, 'error is re-raised to the caller')
  check(vim.bo[b].modifiable == false, 'restored to false after error')
end)

test('with_modifiable: nesting preserves the outer window', function()
  local b = new_buf()
  local after_inner
  T.with_modifiable(b, function()
    T.with_modifiable(b, function() end)
    after_inner = vim.bo[b].modifiable
  end)
  check(after_inner == true, 'outer window still open after inner window')
  check(vim.bo[b].modifiable == false, 'restored to false after outer window')
end)

test('with_modifiable: invalid buffer returns nil', function()
  local b = new_buf()
  vim.api.nvim_buf_delete(b, { force = true })
  check(T.with_modifiable(b, function() end) == nil, 'nil for invalid buffer')
end)
