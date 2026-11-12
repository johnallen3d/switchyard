-- cmd+k: show the tab picker's own tab, the bare `switchyard` picker (see
-- ../../crates/tab-picker/src/standalone.rs) in the Mac's "agents" session.
-- It finds the tab by its pane's label in any session on this host (the
-- config host, the Mac), and selects it. Without one it opens the tab in
-- the agents session, making the session if there isn't one. On that tab
-- already, cmd+k does nothing. The picker acts on its picks itself and
-- stays open, so the tab stays until it is quit (Escape or ctrl-c), which
-- closes it; the next cmd+k opens a new one.
local util = require("switchyard.float")

local SESSION = "agents"
local LABEL = "switchyard"
local WINDOW = "switchyard"

local function pane_spec()
  return rex.layout.block{
    flavor  = "com.superlogical.terminal.shell",
    label   = LABEL,
    options = { command = util.float_command("switchyard") },
  }
end

-- The picker's tab: its session_id and window_id, or nil. Also the agents
-- session's ID, to open the tab in when there is none.
local function find_tab()
  local list, err = rex.call("session.list")
  if list == nil then error(err, 0) end
  local agents
  for _, session in ipairs(list.sessions or {}) do
    rex.session.attach{ session_id = session.session_id }
    local view = rex.session.view{ session_id = session.session_id }
    if view ~= nil then
      if agents == nil and view.label == SESSION then agents = session.session_id end
      for _, window in ipairs(view.windows or {}) do
        for _, layer in ipairs(window.layers or {}) do
          if layer.kind == "tiled" then
            for _, block in ipairs(layer.blocks or {}) do
              if block.label == LABEL then
                return { session_id = session.session_id, window_id = window.window_id,
                  active = view.active_window_id == window.window_id }
              end
            end
          end
        end
      end
    end
  end
  return nil, agents
end

local function open_tab(agents)
  if agents == nil then
    local created, err = rex.call("session.create", {
      label = SESSION,
      initial_windows = { { window_label = WINDOW, layout = pane_spec() } },
    })
    if created == nil then error(err, 0) end
    local first = (created.initial_windows or {})[1] or {}
    return { session_id = created.session_id, window_id = first.window_id }
  end
  local window, err = rex.call("session.new_window", {
    session_id = agents, window_label = WINDOW, focus = true, layout = pane_spec(),
  })
  if window == nil then error(err, 0) end
  return { session_id = agents, window_id = window.window_id }
end

rex.action{
  name  = "tab_picker",
  title = "Go to Tab…",
  run   = function(ctx, args)
    local tab, agents = find_tab()
    if tab ~= nil and tab.session_id == ctx.session_id and tab.active then return end
    tab = tab or open_tab(agents)
    rex.client.queue("session.select", { session_id = tab.session_id, window_id = tab.window_id })
  end,
}

rex.bind("cmd+k", "tab_picker")
