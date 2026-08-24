local root = vim.fn.getcwd()
vim.opt.runtimepath:prepend(root)

local server = os.getenv("COMPOSER_SUPPORT_TEST_SERVER")
  or vim.fs.joinpath(root, "target", "debug", "composer-language-server")
assert(vim.fn.executable(server) == 1, ("language server is not executable: %s"):format(server))

local project = vim.fn.tempname()
vim.fn.mkdir(vim.fs.joinpath(project, "vendor", "composer"), "p")
local composer_path = vim.fs.joinpath(project, "composer.json")
vim.fn.writefile({
  "{",
  '  "require": {',
  '    "inertiajs/inertia-laravel": "^3.0"',
  "  }",
  "}",
}, composer_path)
vim.fn.writefile({
  '{"packages":[{"name":"inertiajs/inertia-laravel","version":"3.2.1"}]}',
}, vim.fs.joinpath(project, "vendor", "composer", "installed.json"))

require("composer_support").setup({
  auto_install = false,
  check_updates = false,
  open_key = false,
  server_path = server,
})
vim.cmd.edit(vim.fn.fnameescape(composer_path))

local attached = vim.wait(5000, function()
  return #vim.lsp.get_clients({ bufnr = 0, name = "composer-language-server" }) == 1
end, 20)
assert(attached, "composer-language-server did not attach")

local text_document = vim.lsp.util.make_text_document_params(0)
local links = vim.lsp.buf_request_sync(0, "textDocument/documentLink", {
  textDocument = text_document,
}, 5000)
local package_link
for _, response in pairs(links or {}) do
  for _, link in ipairs(response.result or {}) do
    if link.target == "https://packagist.org/packages/inertiajs/inertia-laravel" then
      package_link = link
    end
  end
end
assert(package_link, "Packagist document link was not returned")

local hints = vim.lsp.buf_request_sync(0, "textDocument/inlayHint", {
  textDocument = text_document,
  range = {
    start = { line = 0, character = 0 },
    ["end"] = { line = vim.api.nvim_buf_line_count(0), character = 0 },
  },
}, 5000)
local installed_hint
for _, response in pairs(hints or {}) do
  for _, hint in ipairs(response.result or {}) do
    if hint.label == "v3.2.1" then
      installed_hint = hint
    end
  end
end
assert(installed_hint, "installed-version inlay hint was not returned")

for _, client in ipairs(vim.lsp.get_clients({ bufnr = 0, name = "composer-language-server" })) do
  client:stop()
end
vim.fn.delete(project, "rf")
print("Neovim LSP smoke test passed")
