-- One server's rows, run as `rex [-S HOST] do -e SCRIPT host=HOST`, or for
-- the config host `host=local`, which also shows its home as ~. The float
-- (tab-picker.lua) reads this file; rex-tab-picker builds it in for its
-- standalone mode. (`rex do` reads an argument value with a slash as a
-- script path and one like 6e6173 as a number, so the script looks up $HOME
-- and hex-encodes the label itself.) With `status=yes`, as switchyard-server
-- runs it, each tab's T row is followed by a W row with its agent status in
-- full, for the task registry, and a final B row counts the host's busy
-- panes, for choosing a host (see the end).
local terminal = "com.superlogical.terminal"
local server, host = rex, rex.args.host
local home = host == "local" and rex.exec({ "sh", "-c", 'printf %s "$HOME"' }).stdout or ""
-- Hex keeps arbitrary host labels safe inside the terminal-title payload.
local target = host == "local" and "-" or host:gsub(".", function(c) return string.format("%02x", c:byte()) end)
local groups = {}
-- Agents report status per pane; a tab shows its most urgent one. Idle
-- counts only for the W row: a tab whose agents are all idle shows none.
local urgency = { idle = 1, done = 2, working = 3, error = 4, blocked = 5 }
local with_status = rex.args.status == "yes"
-- Panes with an agent working or blocked, as hosts.rs's probe counts them.
local busy = 0
do
  local list, err = server.call("session.list")
  if list == nil then error(host .. ": " .. tostring(err), 0) end
  for _, session in ipairs(list.sessions or {}) do
    local _, attach_err = server.session.attach{ session_id = session.session_id }
    if attach_err ~= nil then error(attach_err, 0) end
    local view, view_err = server.session.view{ session_id = session.session_id }
    if view == nil then error(view_err, 0) end
    local tabs = {}
    groups[#groups + 1] = { session = session, tabs = tabs, host = host, target = target }
    for _, window in ipairs(view.windows or {}) do
      local blocks = {}
      for _, layer in ipairs(window.layers or {}) do
        if layer.kind == "tiled" then
          for _, block in ipairs(layer.blocks or {}) do
            blocks[#blocks + 1] = block.block_id
          end
        end
      end
      local block_id = blocks[1]
      local best
      for _, id in ipairs(blocks) do
        if id == window.focused_block_id then block_id = id end
        local status = server.block.call(terminal, "program_status",
          { session_id = session.session_id, block_id = id })
        local pane_busy = false
        for _, record in ipairs((status and status.records) or {}) do
          if best == nil or (urgency[record.state] or 0) > (urgency[best.state] or 0) then
            best = record
          end
          pane_busy = pane_busy or record.state == "working" or record.state == "blocked"
        end
        if pane_busy then busy = busy + 1 end
      end
      local state = best and (urgency[best.state] or 0) > urgency.idle and best.state or nil
      local args = { session_id = session.session_id, block_id = block_id }
      local name = window.label or ""
      local dir = ""
      if block_id ~= nil then
        local title = server.block.call(terminal, "title", args)
        if name:match("^Window %x+$") and title and title.title ~= "" then
          name = title.title
        end
        local pwd = server.block.call(terminal, "pwd", args)
        dir = ((pwd and pwd.pwd) or ""):gsub("^file://[^/]*", "")
        if home ~= "" and dir:sub(1, #home) == home then
          dir = "~" .. dir:sub(#home + 1)
        end
      end
      tabs[#tabs + 1] = { window_id = window.window_id, name = name, dir = dir, state = state, best = best or {} }
    end
  end
end

-- rex-tab-picker draws the rows; see its source for the format. A W row
-- is WINDOW_ID, STATE, UPDATED_AT (RFC 3339) and MSG, empty for a tab with
-- no status, as the registry's status fields want them (see
-- crates/core/src/tasks.rs). The B row is the busy pane count.
local function clean(s) return (tostring(s or ""):gsub("[\t\r\n]", " ")) end
local lines = {}
for _, group in ipairs(groups) do
  local session = group.session
  lines[#lines + 1] = table.concat({
    "S", group.target, clean(group.host), session.session_id, clean(session.label or session.session_id),
  }, "\t")
  for _, tab in ipairs(group.tabs) do
    lines[#lines + 1] = table.concat({
      "T", tab.window_id, tab.state or "", clean(tab.name), clean(tab.dir),
    }, "\t")
    if with_status then
      lines[#lines + 1] = table.concat({
        "W", tab.window_id, clean(tab.best.state), clean(tab.best.updated_at), clean(tab.best.msg),
      }, "\t")
    end
  end
end
if with_status then lines[#lines + 1] = "B\t" .. busy end
return table.concat(lines, "\n") .. "\n"
