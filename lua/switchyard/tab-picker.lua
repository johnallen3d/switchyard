-- The cmd+k picker: every tab across sessions and hosts, the agents among
-- them, and ended tasks, in one rex-tab-picker float (cmd+i opens it on the
-- agents view). The float opens at once, waiting, on the focused session's
-- host; a background `rex do` on the config host fills it (see
-- util.open_filled_float).
--
-- It asks switchyard-server first: `switchyard-server pastes` prints the
-- pastes below from the server's snapshot in milliseconds (see
-- ../../crates/core/src/server.rs). Without a server, or when it fails, it
-- fans out instead, running at once:
--
-- - one `rex do` per server listing its tabs (each attaching to all its
--   sessions), delivered together as `rows`, with an E row naming each host
--   that failed (`error` when they all did);
-- - `rex-spawn --tasks --live`, the task registry with each tab's live agent
--   status (`live`), and `rex-spawn --tasks`, which also checks each task's
--   worktree over SSH and ends the gone ones (`tasks`); a registry failure
--   is `tasks-error`, and the float shows tabs only.
--
-- The picker records its pick in the terminal title as
-- rex-tab-picker:ACTION:TARGET/SESSION/WINDOW, which the tab_picker mode
-- reads: Enter jumps (go, task), resumes an ended task's agent session in a
-- new tab and jumps there (resume), or runs an archive armed with ctrl+x in
-- the float (archive, force-archive; for ask, a dirty worktree, it types
-- ctrl+x back so the float asks "are you sure"); ctrl+r reviews a task, and
-- ctrl+l opens its main work item (`rex-spawn --act link`). A task with
-- several links adds their count, ACTION:TARGET/SESSION/WINDOW/N, and
-- ctrl+l types ctrl+l back so the float opens its link chooser; while that
-- is open the title is rex-tab-picker:link:URL (hex-encoded), Enter or
-- ctrl+l opens URL and Escape types Escape back to close the chooser.
-- TARGET is "-" for this host or the hex-encoded host label. See
-- ../../crates/tab-picker/src/main.rs and
-- ../../crates/core/src/actions.rs.
local util = require("switchyard.float")

local label = "tab-picker"

-- One server's rows: host_rows.lua beside this file, which rex-tab-picker
-- also builds in for its standalone mode, run as `rex [-S HOST] do -e SCRIPT
-- host=HOST`, or for the config host `host=local`.
local host_rows_script
do
  local source = debug.getinfo(1, "S").source:gsub("^@", "")
  local path = (source:match("^(.*)/[^/]*$") or ".") .. "/host_rows.lua"
  local file, err = io.open(path, "r")
  if file == nil then error("tab-picker: " .. tostring(err), 0) end
  host_rows_script = file:read("*a")
  file:close()
end

-- The rest of the background script. The fan-out's tab listings and task
-- fetches start together; rows go out first, then the tasks as they come.
local fill_script = "local host_rows_script = " .. string.format("%q", host_rows_script) .. [==[

local function deliver_result(kind, r, err)
  if r == nil then
    deliver("tasks-error", tostring(err or "can't run rex-spawn"))
  elseif not r.ok then
    deliver("tasks-error", (r.stderr:gsub("^rex%-spawn: ", "")))
  else
    deliver(kind, r.stdout)
  end
end

-- The pastes `switchyard-server pastes [--checked]` prints, or nil when no
-- server answered in time (or none is installed).
local function server_pastes(...)
  local r = rex.exec({ "switchyard-server", "pastes", ... })
  if r == nil or not r.ok then return nil end
  local ok, pastes = pcall(rex.json.decode, r.stdout)
  if not ok or type(pastes) ~= "table" then return nil end
  return pastes
end

-- From the server: one snapshot, which normally has checked tasks. Only in
-- the server's first moments are they `live`; then a second call waits
-- (briefly) for the check rather than leave the float on live status, and
-- if the server can't say, rex-spawn checks as the fan-out would.
local function from_server()
  local pastes = server_pastes()
  if pastes == nil then return false end
  local pending = false
  for _, p in ipairs(pastes) do
    deliver(p.kind, p.body)
    if p.kind == "live" then pending = true end
    if p.kind == "tasks-error" then pending = false end
  end
  if pending then
    local checked = server_pastes("--checked")
    if checked == nil then
      deliver_result("tasks", rex.exec({ "rex-spawn", "--tasks" }))
    else
      for _, p in ipairs(checked) do deliver(p.kind, p.body) end
    end
  end
  return true
end

local function fan_out()
  local jobs = { { host = "local", job = rex.exec.start({ "rex", "do", "-e", host_rows_script, "host=local" }) } }
  for _, host in ipairs(rex.servers()) do
    jobs[#jobs + 1] = { host = host, job = rex.exec.start({ "rex", "-S", host, "do", "-e", host_rows_script,
      "host=" .. host }) }
  end
  local steps = {}
  for _, step in ipairs({ { "live", "--live" }, { "tasks" } }) do
    local job, err = rex.exec.start({ "rex-spawn", "--tasks", step[2] })
    steps[#steps + 1] = { kind = step[1], job = job, err = err }
  end
  -- A host that fails gets an E row, so the float shows the rest and names
  -- it, as from the server.
  local rows, failed, listed = {}, nil, false
  for _, j in ipairs(jobs) do
    local r = j.job:wait()
    if r.ok then
      listed = true
      rows[#rows + 1] = rex.json.decode(r.stdout)
    else
      local err = j.host .. ": " .. (r.stderr:gsub("%s+$", ""))
      failed = failed or err
      rows[#rows + 1] = "E\t" .. j.host .. "\t" .. err:gsub("[\t\n]", " ") .. "\n"
    end
  end
  if listed then deliver("rows", table.concat(rows)) else deliver("error", failed) end
  for _, step in ipairs(steps) do
    deliver_result(step.kind, step.job and step.job:wait(), step.err)
  end
end

main(function()
  if not from_server() then fan_out() end
end)
]==]

local function close_picker(ctx) util.close_float(ctx, label) end

local function host_label()
  return (rex.exec({ "hostname", "-s" }).stdout:gsub("%s+$", ""))
end

-- An action that waits for the picker holds up the app's keys, so this one
-- opens the float and returns. The picker records the highlighted row, and
-- the tab_picker mode turns Enter, ctrl+r and Escape into the actions that
-- finish. A second press of either shortcut closes the float.
rex.mode("tab_picker")

local function open_picker(ctx, view)
  if util.find_float(ctx, label) ~= nil then return close_picker(ctx) end
  -- Start on the current tab. Key actions carry no window_id, so fall back
  -- to the session's active tab.
  local session_view = ctx.window_id == nil and rex.session.view{ session_id = ctx.session_id } or nil
  local current = ctx.session_id .. "/" .. (ctx.window_id or (session_view and session_view.active_window_id) or "")
  util.open_filled_float(ctx, {
    label    = label,
    mode     = "tab_picker",
    bounds   = { 0.12, 0.1, 0.76, 0.75 },
    command  = util.float_command("rex-tab-picker", "--current", current, "--view", view, "--wait"),
    script   = fill_script,
    log_name = "tab-picker.log",
  })
end

rex.action{
  name  = "tab_picker",
  title = "Go to Tab…",
  run   = function(ctx, args) open_picker(ctx, "all") end,
}

rex.action{
  name  = "tab_picker_agents",
  title = "Agents…",
  run   = function(ctx, args) open_picker(ctx, "agents") end,
}

local function float_title(ctx, block_id)
  local t = rex.block.call("com.superlogical.terminal", "title", {
    session_id = ctx.session_id, block_id = block_id,
  })
  return (t and t.title) or ""
end

-- The float's pick: action, host (the config host's label for "-"),
-- session, window and how many links ctrl+l chooses from; or, with the
-- link chooser open, action "link" and its url. Keys pressed with the float
-- not focused fall through to the pane.
local function read_pick(ctx, key)
  local _, block_id = util.find_float(ctx, label)
  if block_id == nil or block_id ~= ctx.block_id then
    close_picker(ctx)
    rex.client.queue("pane.send_key", { key = key })
    return nil
  end
  local title = float_title(ctx, block_id)
  local url = title:match("^rex%-tab%-picker:link:(%x+)$")
  if url ~= nil then return { action = "link", url = util.unhex(url), float = block_id } end
  local action, target, session_id, window_id, links = title
    :match("^rex%-tab%-picker:([%a%-]+):([^/]+)/([^/]+)/([^/]+)/?(%d*)$")
  if action == nil then return nil end
  local here = target == "-"
  return { action = action, host = here and host_label() or util.unhex(target), here = here,
    session_id = session_id, window_id = window_id, links = tonumber(links) or 1, float = block_id }
end

-- Type keys into the float, for the picker to act on.
local function type_into(ctx, block_id, data)
  rex.block.call("com.superlogical.terminal", "write", {
    session_id = ctx.session_id, block_id = block_id, data = data,
  })
end

-- The Rex app runs on this config host, so a browser opened here opens
-- where John is; Rex has no client action for a URL.
local function open_url(url)
  local r, err = rex.exec({ "sh", "-c", 'if command -v open >/dev/null 2>&1; then exec open "$1"; else exec xdg-open "$1"; fi',
    "sh", url }, { env = util.cli_env() })
  if r == nil then error(err, 0) end
  if not r.ok then error("can't open " .. url .. ": " .. (r.stderr:gsub("%s+$", "")), 0) end
end

local function jump(pick)
  if pick.here or pick.host == host_label() then
    rex.client.queue("session.select", { session_id = pick.session_id, window_id = pick.window_id })
    return
  end
  util.select_on_host(pick.host, "tab-picker.log", util.cli_env(), pick.session_id, pick.window_id)
end

-- Run `rex-spawn --act` for the pick and return its output; a refusal (such
-- as a dirty worktree) comes back as the action's error.
local function act(action, pick)
  local r, err = rex.exec({ "rex-spawn", "--act", action, pick.host, pick.session_id, pick.window_id },
    { env = util.cli_env() })
  if r == nil then error(err, 0) end
  if not r.ok then error((r.stderr:gsub("^rex%-spawn: ", "")), 0) end
  return r.stdout
end

-- Resume prints the new tab as HOST<TAB>SESSION_ID<TAB>WINDOW_ID.
local function resume(pick)
  local host, session_id, window_id = act("resume", pick):match("^([^\t]+)\t([^\t]+)\t([^\t\n]+)")
  if host == nil then error("rex-spawn --act resume printed no tab", 0) end
  jump({ host = host, session_id = session_id, window_id = window_id })
end

rex.action{
  name  = "tab_picker_go",
  title = "Go to, Archive or Resume Picked Row",
  run   = function(ctx, args)
    local pick = read_pick(ctx, "enter")
    -- Nothing picked (an empty list); keep the float open.
    if pick == nil then return end
    -- A dirty worktree: keep the float open and type ctrl-x into it, which
    -- moves it on to "are you sure"; the next Enter answers force-archive.
    if pick.action == "ask" then return type_into(ctx, pick.float, "\24") end
    close_picker(ctx)
    -- The link chooser's pick.
    if pick.action == "link" then return open_url(pick.url) end
    if pick.action == "archive" or pick.action == "force-archive" then return act(pick.action, pick) end
    if pick.action == "resume" then return resume(pick) end
    jump(pick)
  end,
}

-- Review opens lazygit beside the highlighted task's agent, then shows its tab.
rex.action{
  name  = "tab_picker_review",
  title = "Review Picked Task",
  run   = function(ctx, args)
    local pick = read_pick(ctx, "ctrl+r")
    -- Only open tasks have something to review.
    if pick == nil or pick.action ~= "task" then return end
    close_picker(ctx)
    act("review", pick)
    jump(pick)
  end,
}

-- Open the highlighted task's work item: its only one (its PR, else an
-- issue, card or commit; see ../../crates/core/src/links.rs), or, with
-- several, type ctrl+l back so the float offers a chooser.
rex.action{
  name  = "tab_picker_open_link",
  title = "Open Picked Task's Work Item",
  run   = function(ctx, args)
    local pick = read_pick(ctx, "ctrl+l")
    -- Only task rows have work items.
    if pick == nil or pick.action == "go" then return end
    if pick.action == "link" then
      close_picker(ctx)
      return open_url(pick.url)
    end
    if pick.links > 1 then return type_into(ctx, pick.float, "\12") end
    local url = (act("link", pick):gsub("%s+$", ""))
    close_picker(ctx)
    open_url(url)
  end,
}

-- Escape closes the float, or with the link chooser open, types Escape
-- back so the float closes just the chooser.
rex.action{
  name  = "tab_picker_cancel",
  title = "Close Tab Picker",
  run   = function(ctx, args)
    local _, block_id = util.find_float(ctx, label)
    if block_id ~= nil and float_title(ctx, block_id):match("^rex%-tab%-picker:link:") then
      return type_into(ctx, block_id, "\27")
    end
    close_picker(ctx)
  end,
}

rex.bind("cmd+k", "tab_picker")
rex.bind("cmd+i", "tab_picker_agents")
rex.bind("tab_picker/enter", "tab_picker_go")
rex.bind("tab_picker/ctrl+r", "tab_picker_review")
rex.bind("tab_picker/ctrl+l", "tab_picker_open_link")
rex.bind("tab_picker/escape", "tab_picker_cancel")
