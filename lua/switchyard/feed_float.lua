-- The config host's end of a cmd+k quick jump float on another host (see
-- tab-picker.lua and crates/tab-picker/src/fed.rs). `switchyard float feed`
-- runs it as `rex do -e SCRIPT session=SESSION block=BLOCK` naming the
-- float, and switchyard builds it in.
--
-- It finds the float's server and prints one line: `local` when the float
-- is on this server (where it fetches for itself) and the script ends, or
-- `host<TAB>LABEL`. Then it reads frames from stdin, each a line
-- "KIND LENGTH" and LENGTH bytes of body, and types each into the float as
-- the data paste "rex-data:KIND\n" .. body. KIND `ping` carries nothing and
-- only checks that the float is still there. It ends once the float or its
-- stdin has gone.
local T = "com.superlogical.terminal"
local float = { session_id = rex.args.session, block_id = rex.args.block }

local function say(line)
  io.write(line, "\n")
  io.stdout:flush()
end

local _, local_err = rex.session.attach{ session_id = float.session_id }
if local_err == nil then
  say("local")
  return ""
end

-- Every server is asked at once; the first that has the session is the
-- float's.
local lookups = {}
for _, host in ipairs(rex.servers()) do
  lookups[#lookups + 1] = { host = host, job = rex.exec.start({ "rex", "-S", host, "do", "-e",
    -- `rex do` prints nothing for a boolean, so answer with a string.
    'local _, err = rex.session.attach{ session_id = rex.args.session } return err == nil and "found" or ""',
    "session=" .. float.session_id }) }
end
local server, host
for _, l in ipairs(lookups) do
  if (l.job:wait().stdout or ""):match("found") then
    server, host = rex.server(l.host), l.host
    break
  end
end
if server == nil then error("can't find the float's session " .. float.session_id, 0) end
local _, attach_err = server.session.attach{ session_id = float.session_id }
if attach_err ~= nil then error(attach_err, 0) end
say("host\t" .. host)

-- A call on the float's block; nil once the float has gone, which Rex
-- reports as an error.
local function call(method, args)
  local ok, result = pcall(server.block.call, T, method, args)
  if ok then return result end
end

-- Bytes of paste body per terminal write: under Rex's limit even if the
-- JSON request escapes every byte as \u00XX.
local CHUNK = 8 * 1024

-- Type one data paste into the float; false once the float has gone.
local function deliver(kind, body)
  -- Wait (up to 5s) for the picker to take the terminal raw, so the paste
  -- isn't echoed or line-edited.
  for _ = 1, 100 do
    local mode = call("input_mode", float)
    if mode == nil then return false end
    if not mode.canonical and not mode.echo then break end
    rex.sleep(0.05)
  end
  local function write(data)
    return call("write", { session_id = float.session_id, block_id = float.block_id, data = data }) ~= nil
  end
  -- Rex drops a write over about 64 KiB encoded, so the paste goes out in
  -- pieces the picker's reader joins up again: start marker and header, the
  -- body in chunks, then the end marker. An escape in the body could end
  -- the paste early.
  body = body:gsub("\27", "")
  local ok = write("\27[200~rex-data:" .. kind .. "\n")
  local i = 1
  while ok and i <= #body do
    local j = math.min(i + CHUNK - 1, #body)
    -- Cut on a UTF-8 boundary: back off while the next byte continues a character.
    while j < #body and j > i and body:byte(j + 1) >= 0x80 and body:byte(j + 1) < 0xC0 do j = j - 1 end
    ok = write(body:sub(i, j))
    i = j + 1
  end
  -- Close the paste even after a failure, so what follows isn't swallowed.
  ok = write("\27[201~") and ok
  local there = call("input_mode", float) ~= nil
  if not ok and there then
    io.stderr:write("switchyard: can't deliver ", kind, " (", #body, " bytes) to the float\n")
  end
  return there
end

while true do
  local header = io.read("*l")
  if header == nil then return "" end
  local kind, length = header:match("^(%S+) (%d+)$")
  if kind == nil then error("bad frame header: " .. header, 0) end
  length = tonumber(length)
  local body = length > 0 and (io.read(length) or "") or ""
  if kind == "ping" then
    if call("input_mode", float) == nil then return "" end
  elseif not deliver(kind, body) then
    return ""
  end
end
