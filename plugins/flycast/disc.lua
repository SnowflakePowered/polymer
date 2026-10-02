local request = os.getenv("POLYMER_DISC_REQUEST")
local current
local pending
local frames = 0

local function requested()
    local file = request and io.open(request, "r")
    if not file then return nil end
    local path = file:read("*l")
    file:close()
    return path
end

flycast_callbacks = {
    overlay = function()
        frames = frames + 1
        if pending then
            if frames >= 60 then
                flycast.emulator.insertGdrom(pending)
                current, pending = pending, nil
            end
            return
        end
        if frames % 30 ~= 0 then return end
        local path = requested()
        if not path or path == "" or path == current then return end
        if current == nil then
            current = path
            return
        end
        pending, frames = path, 0
        flycast.emulator.openGdrom()
    end,
}
