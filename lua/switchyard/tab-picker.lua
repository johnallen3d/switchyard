-- cmd+k: the quick jump float, `switchyard --quick` over the focused tab:
-- Inbox's list alone, with the tab cmd+k was pressed on highlighted. Enter
-- shows the highlighted tab and Escape closes it; either way the picker
-- quits, which closes the float. It needs no key mode, and it runs on the
-- focused session's host. On the Mac it fetches and acts on its own. Only
-- the Mac knows every Rex server and has the task registry, so elsewhere it
-- runs with --fed-by naming the Mac and fetches nothing: a detached
-- `switchyard float feed` here pastes the Mac's tabs and tasks into it and
-- acts on the pick it reports (see ../../crates/tab-picker/src/fed.rs),
-- logging to quick-jump.log. cmd+k over it closes it.
--
-- cmd+i: show the tab picker's own tab, Inbox, the bare `switchyard` picker
-- (see ../../crates/tab-picker/src/standalone.rs) in the Mac's "agents"
-- session.
-- It finds the tab by its pane's label in any session on this host (the
-- config host, the Mac), and selects it. Without one it opens the tab in
-- the agents session, making the session if there isn't one (and moving
-- it to the top of the session list, as a new session goes last). On that tab
-- already, cmd+i does nothing. The picker acts on its picks itself and
-- stays open, and no key quits it: the tab stays until it is closed
-- like any other (cmd+w); the next cmd+i opens a new one.
--
-- An action can only view (or add windows to) sessions its connection is
-- attached to, and rex.session.attach is for scripts, so the finding and
-- opening run as a `rex do` script (FIND_OR_OPEN), which attaches to each
-- session. It prints the tab as JSON: session_id, window_id, and whether
-- that is its session's active window.
--
-- The picker highlights the tab cmd+i was pressed on, as the float does: the
-- script gets it as session= and window= (a key action has no window, so
-- the session's active one), and passes it to a new picker as --current
-- SESSION/WINDOW, or pastes it into a running one as rex-data:current,
-- which also clears the running picker's search.
local util = require("switchyard.float")

local FIND_OR_OPEN = "local COMMAND = " .. string.format("%q", rex.json.encode(util.float_command("switchyard"))) .. [==[

local SESSION, LABEL, WINDOW = "agents", "switchyard", "switchyard"
local T = "com.superlogical.terminal"
COMMAND = rex.json.decode(COMMAND)

-- The tab cmd+i was pressed on, as SESSION/WINDOW, or nil.
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
            -- Not when cmd+i is pressed on the picker's own tab.
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

local function show_inbox(ctx)
  local tab = M.find_or_open(ctx.session_id, ctx.window_id)
  if tab.session_id == ctx.session_id and tab.active then return end
  rex.client.queue("session.select", { session_id = tab.session_id, window_id = tab.window_id })
end

rex.action{
  name  = "inbox",
  title = "Go to Inbox",
  run   = function(ctx, args) show_inbox(ctx) end,
}

local quick_label = "quick-jump"

rex.action{
  name  = "quick_jump",
  title = "Go to Tab…",
  run   = function(ctx, args)
    -- With no session to float over, Inbox does the same job.
    if ctx.session_id == nil then return show_inbox(ctx) end
    if util.find_float(ctx, quick_label) ~= nil then return util.close_float(ctx, quick_label) end
    -- A key action has no window: the session's active one.
    local window = ctx.window_id
    if window == nil or window == "" then
      local view = rex.session.view{ session_id = ctx.session_id }
      window = view and view.active_window_id
    end
    local host = (rex.exec({ "hostname", "-s" }).stdout or ""):gsub("%s+$", "")
    local command = util.float_command("switchyard", "--quick", "--fed-by", host)
    if window ~= nil and window ~= "" then
      command[#command + 1] = "--current"
      command[#command + 1] = ctx.session_id .. "/" .. window
    end
    local layer, err = rex.session.new_layer{
      session_id = ctx.session_id,
      bounds     = { 0.15, 0.1, 0.7, 0.75 },
      focus      = true,
      layout     = rex.layout.block{
        flavor  = "com.superlogical.terminal.shell",
        label   = quick_label,
        options = { command = command },
      },
    }
    if layer == nil then error(err, 0) end
    -- The feed finds the float's host, and ends at once when it is this one.
    local block_id = (layer.block_ids or {})[1]
    if block_id == nil then error("new float has no block", 0) end
    local job, job_err = rex.exec.start({
      "sh", "-c", 'umask 077; log="${XDG_STATE_HOME:-$HOME/.local/state}/rex/quick-jump.log"; mkdir -p "${log%/*}" || exit; exec "$@" >> "$log" 2>&1',
      "sh", "switchyard", "float", "feed", ctx.session_id, block_id,
    }, { detach = true, env = util.cli_env() })
    if job == nil then error(job_err, 0) end
  end,
}

rex.bind("cmd+i", "inbox")
rex.bind("cmd+k", "quick_jump")

-- cmd+1-5: on the picker's own tab, show Inbox, Work, Usage, Archive or Search
-- (pasted as rex-data:view); anywhere else, Rex's own Go to Tab. A
-- function binding that declines sends the key to the terminal rather
-- than to the default, so it runs Go to Tab itself.
local VIEWS = { "inbox", "work", "usage", "archive", "search" }

-- Whether the focused block is the picker's tab (labelled as FIND_OR_OPEN
-- labels it; the quick float is a floating layer, not a tiled one).
local function on_picker(ctx)
  if ctx.session_id == nil or ctx.block_id == nil then return false end
  local view = rex.session.view{ session_id = ctx.session_id }
  for _, window in ipairs((view and view.windows) or {}) do
    for _, layer in ipairs(window.layers or {}) do
      for _, block in ipairs(layer.blocks or {}) do
        if block.block_id == ctx.block_id then
          return layer.kind == "tiled" and block.label == "switchyard"
        end
      end
    end
  end
  return false
end

for index, name in ipairs(VIEWS) do
  rex.bind("cmd+" .. index, function(ctx, ev)
    if on_picker(ctx) then
      rex.block.call("com.superlogical.terminal", "write", {
        session_id = ctx.session_id, block_id = ctx.block_id,
        data = "\27[200~rex-data:view\n" .. name .. "\27[201~",
      })
    else
      rex.client.queue("client.tab.goto", { index = index })
    end
    return true
  end)
end

return M
