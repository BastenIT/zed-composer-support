local root = vim.fn.getcwd()
vim.opt.runtimepath:prepend(root)

local composer = require("composer_support")
assert(composer._server_version == "0.2.4")

local platforms = {
  { "Darwin", "x86_64", "composer-language-server-x86_64-apple-darwin" },
  { "Darwin", "arm64", "composer-language-server-aarch64-apple-darwin" },
  { "Linux", "x86_64", "composer-language-server-x86_64-unknown-linux-musl" },
  { "Linux", "aarch64", "composer-language-server-aarch64-unknown-linux-musl" },
  { "Windows_NT", "AMD64", "composer-language-server-x86_64-pc-windows-msvc.exe" },
  { "Windows_NT", "ARM64", "composer-language-server-aarch64-pc-windows-msvc.exe" },
}
for _, case in ipairs(platforms) do
  local asset, err = composer._platform_asset(case[1], case[2])
  assert(not err, err)
  assert(asset == case[3], ("expected %s, got %s"):format(case[3], asset))
end
local unsupported, unsupported_error = composer._platform_asset("Plan9", "mips")
assert(not unsupported)
assert(unsupported_error:match("unsupported platform"))

dofile(vim.fs.joinpath(root, "plugin", "composer_support.lua"))
assert(vim.g.loaded_composer_support == 1)
composer.setup({ auto_install = false, open_key = false })
assert(vim.fn.exists(":ComposerSupportInstall") == 2)
assert(vim.fn.exists(":ComposerSupportInfo") == 2)
assert(vim.fn.exists(":ComposerOpenPackage") == 2)

print("Neovim integration checks passed")
