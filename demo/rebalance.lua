-- Rebalance the hot wallet to 60:40 against the vault, but only if ETH/USD has
-- moved more than 2% in the last 24 hours.
--
-- The VM has no floats, so prices arrive as decimal strings and the move is
-- computed in basis points from the digits. Balances and amounts are integers
-- in milli-ETH.

local HOT = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
local VAULT = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
local HOT_SHARE = 60
local THRESHOLD_BPS = 200

-- "2550.75" -> 255075: the price in hundredths, read one digit at a time.
local hundredths = function(text)
  local scaled = 0
  local decimals = 0
  local seen_point = false
  local length = string.len(text)
  local i = 1
  while i <= length do
    local byte = string.byte(text, i)
    if byte == 46 then
      seen_point = true
    elseif seen_point then
      if decimals < 2 then
        scaled = scaled * 10 + (byte - 48)
        decimals = decimals + 1
      end
    else
      scaled = scaled * 10 + (byte - 48)
    end
    i = i + 1
  end
  while decimals < 2 do
    scaled = scaled * 10
    decimals = decimals + 1
  end
  return scaled
end

local quote = market.get_price{ pair = "ETH/USD" }
local now = hundredths(quote.price)
local before = hundredths(quote.price_24h_ago)
local moved = now - before
if moved < 0 then
  moved = 0 - moved
end
local moved_bps = (moved * 10000) // before

if moved_bps <= THRESHOLD_BPS then
  return { action = "hold", moved_bps = moved_bps }
end

local hot = wallet.get_balance{ address = HOT }
local vault = wallet.get_balance{ address = VAULT }
local target = ((hot.eth_milli + vault.eth_milli) * HOT_SHARE) // 100
local amount = hot.eth_milli - target

if amount <= 0 then
  return { action = "hold", moved_bps = moved_bps, amount = amount }
end

local ok, res = pcall(function()
  return wallet.transfer{ to = VAULT, amount = amount }
end)
if not ok then
  return {
    action = "refused",
    moved_bps = moved_bps,
    amount = amount,
    reason = res,
  }
end
return {
  action = "rebalanced",
  moved_bps = moved_bps,
  amount = amount,
  tx_hash = res.tx_hash,
}
