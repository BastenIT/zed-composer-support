local M = {}

function M.check()
  vim.health.start("Composer Support")
  if vim.fn.has("nvim-0.10") == 1 then
    vim.health.ok("Neovim 0.10 or newer")
  else
    vim.health.error("Neovim 0.10 or newer is required")
  end

  if vim.fn.executable("curl") == 1 then
    vim.health.ok("curl is available for automatic language-server installation")
  else
    vim.health.warn("curl is unavailable; configure server_path or install curl")
  end

  local composer = require("composer_support")
  local asset, err = composer._platform_asset(vim.uv.os_uname().sysname, vim.uv.os_uname().machine)
  if asset then
    vim.health.ok(("Native release target: %s"):format(asset))
  else
    vim.health.error(err)
  end
end

return M
