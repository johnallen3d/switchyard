-- cmd+k: show the tab picker's own tab, the bare `switchyard` picker (see
-- ../../crates/tab-picker/src/standalone.rs) in the Mac's "agents" session.
-- It finds the tab by its pane's label in any session on this host (the
-- config host, the Mac), and selects it. Without one it opens the tab in
-- the agents session, making the session if there isn't one (and moving
-- it to the top of the session list, as a new session goes last). On that tab
-- already, cmd+k does nothing. The picker acts on its picks itself and
-- stays open, so the tab stays until it is quit (Escape or ctrl-c), which
-- closes it; the next cmd+k opens a new one.
--
-- An action can only view (or add windows to) sessions its connection is
-- attached to, and rex.session.attach is for scripts, so the finding and
-- opening run as a `rex do` script (FIND_OR_OPEN), which attaches to each
-- session. It prints the tab as JSON: session_id, window_id, and whether
-- that is its session's active window.
--
-- The picker highlights the tab cmd+k was pressed on, as the float did: the
-- script gets it as session= and window= (a key action has no window, so
-- the session's active one), and passes it to a new picker as --current
-- SESSION/WINDOW, or pastes it into a running one as rex-data:current,
-- which also clears the running picker's search.
local util = require("switchyard.float")

local FIND_OR_OPEN = "local COMMAND = " .. string.format("%q", rex.json.encode(util.float_command("switchyard"))) .. [==[

local SESSION, LABEL, WINDOW = "agents", "switchyard", "switchyard"
local T = "com.superlogical.terminal"
COMMAND = rex.json.decode(COMMAND)

-- The tab cmd+k was pressed on, as SESSION/WINDOW, or nil.
local current
do
  local session, window = rex.args.session, rex.args.window
  if session ~= nil and session ~= "" then
    if window == nil or window == "" then
      local _, attach_err = rex.session.attach{ session_id = session }
      local view = attach_err == nil and rex.session.view{ session_id = session } or nil
      window = view and view.active_window_id
    end
    if window ~= nil and window ~= "" then current = session .. "/" .. window end
  end
end

local function pane_spec()
  local command = { unpack(COMMAND) }
  if current ~= nil then
    command[#command + 1] = "--current"
    command[#command + 1] = current
  end
  return rex.layout.block{
    flavor  = "com.superlogical.terminal.shell",
    label   = LABEL,
    options = { command = command },
  }
end

-- Tell a running picker which tab to highlight, and to clear its search
-- (an empty tab just clears it). Best effort: a picker that doesn't take
-- it still shows.
local function paste_current(session_id, block_id)
  rex.block.call(T, "write", { session_id = session_id, block_id = block_id,
    data = "\27[200~rex-data:current\n" .. (current or "") .. "\27[201~" })
end

local list, list_err = rex.call("session.list")
if list == nil then error(list_err, 0) end
local agents
for _, session in ipairs(list.sessions or {}) do
  local _, attach_err = rex.session.attach{ session_id = session.session_id }
  if attach_err ~= nil then error(attach_err, 0) end
  local view, view_err = rex.session.view{ session_id = session.session_id }
  if view == nil then error(view_err, 0) end
  if agents == nil and view.label == SESSION then agents = session.session_id end
  for _, window in ipairs(view.windows or {}) do
    for _, layer in ipairs(window.layers or {}) do
      if layer.kind == "tiled" then
        for _, block in ipairs(layer.blocks or {}) do
          if block.label == LABEL then
            -- Not when cmd+k is pressed on the picker's own tab.
            if current ~= session.session_id .. "/" .. window.window_id then
              paste_current(session.session_id, block.block_id)
            end
            return rex.json.encode({ session_id = session.session_id, window_id = window.window_id,
              active = view.active_window_id == window.window_id })
          end
        end
      end
    end
  end
end

if agents == nil then
  local created, err = rex.call("session.create", {
    label = SESSION,
    initial_windows = { { window_label = WINDOW, layout = pane_spec() } },
  })
  if created == nil then error(err, 0) end
  -- A new session goes last; put it first. Best effort: it still opens.
  local top = (list.sessions or {})[1]
  if top ~= nil then
    rex.call("session.move", { session_id = created.session_id, before_session_id = top.session_id })
  end
  local first = (created.initial_windows or {})[1] or {}
  return rex.json.encode({ session_id = created.session_id, window_id = first.window_id, active = true })
end
local window, err = rex.session.new_window{
  session_id = agents, window_label = WINDOW, focus = true, layout = pane_spec(),
}
if window == nil then error(err, 0) end
return rex.json.encode({ session_id = agents, window_id = window.window_id, active = false })
]==]

local M = {}

-- The picker's tab, found or just opened (see FIND_OR_OPEN), highlighting
-- the tab SESSION/WINDOW (WINDOW nil: the session's active one).
function M.find_or_open(session_id, window_id)
  local r, err = rex.exec({ "rex", "do", "-e", FIND_OR_OPEN,
    "session=" .. (session_id or ""), "window=" .. (window_id or "") }, { env = util.cli_env() })
  if r == nil then error(err, 0) end
  if not r.ok then error("can't find or open the switchyard tab: " .. (r.stderr:gsub("%s+$", "")), 0) end
  -- `rex do` prints the returned string as JSON.
  local ok, out = pcall(rex.json.decode, r.stdout)
  if ok and type(out) == "string" then ok, out = pcall(rex.json.decode, out) end
  if not ok or type(out) ~= "table" or out.session_id == nil then
    error("can't read the switchyard tab from: " .. r.stdout, 0)
  end
  return out
end

rex.action{
  name  = "tab_picker",
  title = "Go to Tab…",
  run   = function(ctx, args)
    local tab = M.find_or_open(ctx.session_id, ctx.window_id)
    if tab.session_id == ctx.session_id and tab.active then return end
    rex.client.queue("session.select", { session_id = tab.session_id, window_id = tab.window_id })
  end,
}

rex.bind("cmd+k", "tab_picker")

return M
