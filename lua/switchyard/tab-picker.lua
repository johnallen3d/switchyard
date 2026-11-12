-- The cmd+k picker: every tab across sessions and hosts, with the agents
-- among them under status headings in each session, and ended tasks, in one
-- picker float (`switchyard float tabs`). The float opens at once, waiting,
-- on the focused session's host; a background `rex do` on the config host
-- fills it (see util.open_filled_float).
--
-- It asks switchyard-server first: `switchyard server pastes` prints the
-- pastes below from the server's snapshot in milliseconds (see
-- ../../crates/core/src/server.rs). Without a server, or when it fails, it
-- fans out instead, running at once:
--
-- - one `rex do` per server listing its tabs (each attaching to all its
--   sessions), delivered together as `rows`, with an E row naming each host
--   that failed (`error` when they all did);
-- - `switchyard task list --live`, the task registry with each tab's live agent
--   status (`live`), and `switchyard task list`, which also checks each task's
--   worktree over SSH and ends the gone ones (`tasks`); a registry failure
--   is `tasks-error`, and the float shows tabs only.
--
-- While the float is open, it also fetches the transcript of the agent the
-- title names, for the float's preview (`switchyard transcript`; see the
-- fill script's preview_step).
--
-- The picker records its pick in the terminal title as
-- rex-tab-picker:ACTION:TARGET/SESSION/WINDOW, which the tab_picker mode
-- reads. The mode binds only Enter and Escape, so Rex's key-mode popup stays
-- small; the float's other keys arm what Enter does. Enter jumps (go,
-- task), resumes an ended task's agent session in a new tab and jumps there
-- (resume), or runs what the float armed: an archive with ctrl+x (archive,
-- force-archive; for ask, a dirty worktree, it types ctrl+x back so the
-- float asks "are you sure"), a review with ctrl+r (review), or a pin with
-- ctrl+t (pin; `switchyard task act pin`, then it types Enter back so the
-- float shows it). ctrl+l in the float arms a task's only work item, or
-- opens a chooser over several; either way the title is
-- rex-tab-picker:link:URL (hex-encoded), Enter opens URL, and with the
-- chooser open Escape types Escape back to close it. ctrl+s reaches the
-- float, which opens its snooze prompt; while that is open the title is
-- rex-tab-picker:snooze:UNTIL:TARGET/SESSION/WINDOW (UNTIL in Unix
-- seconds, 0 to wake), or rex-tab-picker:snooze-invalid while the typed
-- time can't be read. Enter snoozes (`switchyard task act snooze ... UNTIL`)
-- and types Enter back so the float shows it; Escape types Escape back to
-- close the prompt. ctrl+y opens the float's reply box on an agent; while
-- it is open the title is
-- rex-tab-picker:message:PROMPT_REF:TARGET/SESSION/WINDOW (PROMPT_REF
-- hex-encoded: the prompt file holding the message on the float's host),
-- or rex-tab-picker:message-empty while there is nothing to send. Enter
-- sends it (`switchyard task act message ... PROMPT_REF`) and types Enter
-- back so the float closes the box; Escape types Escape back to close it.
-- TARGET is "-" for this host or the hex-encoded host label. See
-- ../../crates/tab-picker/src/main.rs and
-- ../../crates/core/src/actions.rs.
local util = require("switchyard.float")

local label = "tab-picker"

-- One server's rows: host_rows.lua beside this file, which switchyard
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
    deliver("tasks-error", tostring(err or "can't run switchyard task list"))
  elseif not r.ok then
    deliver("tasks-error", (r.stderr:gsub("^rex%-spawn: ", "")))
  else
    deliver(kind, r.stdout)
  end
end

-- The pastes `switchyard-server pastes [--checked]` prints, or nil when no
-- server answered in time (or none is installed).
local function server_pastes(...)
  local r = rex.exec({ "switchyard", "server", "pastes", ... })
  if r == nil or not r.ok then return nil end
  local ok, pastes = pcall(rex.json.decode, r.stdout)
  if not ok or type(pastes) ~= "table" then return nil end
  return pastes
end

-- From the server: one snapshot, which normally has checked tasks. Only in
-- the server's first moments are they `live`; then a second call waits
-- (briefly) for the check rather than leave the float on live status, and
-- if the server can't say, `switchyard task list` checks as the fan-out would.
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
      deliver_result("tasks", rex.exec({ "switchyard", "task", "list" }))
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
    local job, err = rex.exec.start({ "switchyard", "task", "list", step[2] })
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

-- The preview (see ../../crates/tab-picker/src/preview.rs): the transcript
-- of the agent in the tab the float's title names, from `switchyard
-- transcript TARGET SESSION WINDOW`, once the highlight has rested a tick,
-- then every PREVIEW_REFRESH seconds while it stays (PREVIEW_BACKOFF after
-- a failure, such as a tab with no agent), one at a time. Each that
-- differs from the last is pasted as `preview`: TARGET/SESSION/WINDOW, a
-- newline, then the JSON.
local PREVIEW_REFRESH, PREVIEW_BACKOFF = 2, 10
local preview = { want = nil, seen = nil, job = nil, key = nil, at = nil, failed = false, sent = nil }

local function title_pane()
  local t = float_server().block.call(T, "title", float)
  local title = (t and t.title) or ""
  -- The link chooser, the snooze prompt and the reply box stay over the
  -- same row.
  if title:match("^rex%-tab%-picker:link:") or title:match("^rex%-tab%-picker:snooze")
    or title:match("^rex%-tab%-picker:message") then return preview.want end
  local action, target, session, window = title:match("^rex%-tab%-picker:([%a%-]+):([^/]+)/([^/]+)/([^/]+)")
  if action == nil or action == "resume" then return nil end
  window = window:gsub("%s+$", "")
  return { key = target .. "/" .. session .. "/" .. window, target = target, session = session, window = window }
end

local function preview_step(now)
  if preview.job ~= nil and not preview.job:running() then
    local r = preview.job:wait()
    local ok, v = pcall(rex.json.decode, r and r.ok and r.stdout or "")
    local body = r and r.ok and r.stdout
      or rex.json.encode({ error = ((r and r.stderr or "can't run switchyard transcript"):gsub("%s+$", "")) })
    preview.failed = not ok or type(v) ~= "table" or v.error ~= nil
    preview.job, preview.at = nil, now
    local paste = preview.key .. "\n" .. body
    if paste ~= preview.sent then
      deliver("preview", paste)
      preview.sent = paste
    end
  end
  local want = title_pane()
  preview.want = want
  local key = want and want.key
  local settled = key ~= nil and key == preview.seen
  preview.seen = key
  if preview.job ~= nil or not settled then return end
  local wait = preview.failed and PREVIEW_BACKOFF or PREVIEW_REFRESH
  if key == preview.key and preview.at ~= nil and now - preview.at < wait then return end
  preview.key, preview.at, preview.failed = key, now, false
  preview.job = rex.exec.start({ "switchyard", "transcript", want.target, want.session, want.window })
  if preview.job == nil then preview.failed = true end
end

local TICK, USAGE_REFRESH = 0.2, 60

main(function()
  -- Accounting is independent of tab/task delivery, including the server
  -- fast path. Never delay opening the picker for a log scan or remote host.
  local usage_job = rex.exec.start({ "switchyard", "usage", "--json" })
  local usage_at
  if not from_server() then fan_out() end
  while true do
    -- Stop with the float rather than leaving a detached polling loop.
    if float_server().block.call(T, "input_mode", float) == nil then return end
    local now = os.time()
    if usage_job == nil and usage_at ~= nil and now - usage_at >= USAGE_REFRESH then
      usage_job, usage_at = rex.exec.start({ "switchyard", "usage", "--json" }), nil
    end
    if usage_at == nil and (usage_job == nil or not usage_job:running()) then
      local r = usage_job and usage_job:wait()
      if r and r.ok then deliver("usage", r.stdout)
      else deliver("usage-error", "Usage collector failed; install matching Switchyard on the config host") end
      usage_job, usage_at = nil, now
    end
    preview_step(now)
    rex.sleep(TICK)
  end
end)
]==]

local function close_picker(ctx) util.close_float(ctx, label) end

local function host_label()
  return (rex.exec({ "hostname", "-s" }).stdout:gsub("%s+$", ""))
end

-- An action that waits for the picker holds up the app's keys, so this one
-- opens the float and returns. The picker records the highlighted row, and
-- the tab_picker mode turns Enter and Escape into the actions that finish. A second press of either shortcut closes the float.
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
    command  = util.float_command("switchyard", "float", "tabs", "--current", current, "--view", view, "--wait"),
    script   = fill_script,
    log_name = "tab-picker.log",
  })
end

rex.action{
  name  = "tab_picker",
  title = "Go to Tab…",
  run   = function(ctx, args) open_picker(ctx, "all") end,
}

local function float_title(ctx, block_id)
  local t = rex.block.call("com.superlogical.terminal", "title", {
    session_id = ctx.session_id, block_id = block_id,
  })
  return (t and t.title) or ""
end

-- The float's pick: action, host (the config host's label for "-"),
-- session and window; or, for a work item to open, action "link" and its
-- url. Keys pressed with the float
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
  local wake_at, starget, ssession, swindow = title:match("^rex%-tab%-picker:snooze:(%d+):([^/]+)/([^/]+)/([^/]+)")
  if wake_at ~= nil then
    local shere = starget == "-"
    return { action = "snooze", wake_at = wake_at, host = shere and host_label() or util.unhex(starget), here = shere,
      session_id = ssession, window_id = swindow, float = block_id }
  end
  local prompt_ref, mtarget, msession, mwindow = title:match("^rex%-tab%-picker:message:(%x+):([^/]+)/([^/]+)/([^/]+)")
  if prompt_ref ~= nil then
    local mhere = mtarget == "-"
    return { action = "message", prompt_ref = util.unhex(prompt_ref), host = mhere and host_label() or util.unhex(mtarget),
      here = mhere, session_id = msession, window_id = (mwindow:gsub("%s+$", "")), float = block_id }
  end
  local action, target, session_id, window_id = title
    :match("^rex%-tab%-picker:([%a%-]+):([^/]+)/([^/]+)/([^/]+)/?%d*$")
  if action == nil then return nil end
  local here = target == "-"
  return { action = action, host = here and host_label() or util.unhex(target), here = here,
    session_id = session_id, window_id = window_id, float = block_id }
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

-- Run `switchyard task act` for the pick and return its output; a refusal (such
-- as a dirty worktree) comes back as the action's error.
local function act(action, pick, ...)
  local r, err = rex.exec({ "switchyard", "task", "act", action, pick.host, pick.session_id, pick.window_id, ... },
    { env = util.cli_env() })
  if r == nil then error(err, 0) end
  if not r.ok then error((r.stderr:gsub("^rex%-spawn: ", "")), 0) end
  return r.stdout
end

-- Resume prints the new tab as HOST<TAB>SESSION_ID<TAB>WINDOW_ID.
local function resume(pick)
  local host, session_id, window_id = act("resume", pick):match("^([^\t]+)\t([^\t]+)\t([^\t\n]+)")
  if host == nil then error("switchyard task act resume printed no tab", 0) end
  jump({ host = host, session_id = session_id, window_id = window_id })
end

rex.action{
  name  = "tab_picker_go",
  title = "Go",
  run   = function(ctx, args)
    local pick = read_pick(ctx, "enter")
    -- Nothing picked (an empty list); keep the float open.
    if pick == nil then return end
    -- A dirty worktree: keep the float open and type ctrl-x into it, which
    -- moves it on to "are you sure"; the next Enter answers force-archive.
    if pick.action == "ask" then return type_into(ctx, pick.float, "\24") end
    -- The snooze prompt: save it, then type Enter back so the float shows
    -- it and moves on.
    if pick.action == "snooze" then
      act("snooze", pick, pick.wake_at)
      return type_into(ctx, pick.float, "\r")
    end
    -- The reply box: send it, then type Enter back so the float closes it.
    -- A refusal (an agent waiting on a prompt) keeps the box open.
    if pick.action == "message" then
      act("message", pick, pick.prompt_ref)
      return type_into(ctx, pick.float, "\r")
    end
    -- An armed pin: save it, then type Enter back so the float shows it.
    if pick.action == "pin" then
      act("pin", pick)
      return type_into(ctx, pick.float, "\r")
    end
    close_picker(ctx)
    -- A work item: armed, or the chooser's pick.
    if pick.action == "link" then return open_url(pick.url) end
    if pick.action == "archive" or pick.action == "force-archive" then return act(pick.action, pick) end
    if pick.action == "resume" then return resume(pick) end
    -- An armed review opens lazygit beside the task's agent, then shows
    -- its tab.
    if pick.action == "review" then act("review", pick) end
    jump(pick)
  end,
}

-- Escape closes the float, or with a work item armed, the link chooser,
-- the snooze prompt or the reply box open, types Escape back so the float
-- cancels just that.
rex.action{
  name  = "tab_picker_cancel",
  title = "Close",
  run   = function(ctx, args)
    local _, block_id = util.find_float(ctx, label)
    local title = block_id ~= nil and float_title(ctx, block_id) or ""
    if title:match("^rex%-tab%-picker:link:") or title:match("^rex%-tab%-picker:snooze")
      or title:match("^rex%-tab%-picker:message") then
      return type_into(ctx, block_id, "\27")
    end
    close_picker(ctx)
  end,
}

rex.bind("cmd+k", "tab_picker")
rex.bind("tab_picker/enter", "tab_picker_go")
rex.bind("tab_picker/escape", "tab_picker_cancel")
