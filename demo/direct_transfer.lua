-- Step 4: a transfer attempted through the `tool.call` primitive rather than
-- the generated `wallet.transfer` wrapper.
--
-- With the tool off the allow-list, the wrapper is gone from the description
-- the model is given. The primitive is still there, and the policy refuses the
-- call at run time.

local VAULT = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"

local ok, res = pcall(function()
  return tool.call("wallet.transfer", { to = VAULT, amount = 5 })
end)
return { ok = ok, reason = res }
