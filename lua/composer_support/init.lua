local M = {}

local SERVER_VERSION = "0.2.4"
local MIN_SERVER_BYTES = 64 * 1024
local CACHE_ENV = "COMPOSER_LANGUAGE_SERVER_CACHE_DIR"
local REPOSITORY = "BastenIT/zed-composer-support"

local defaults = {
  auto_install = true,
  check_updates = true,
  inlay_hints = true,
  open_key = "gx",
  server_path = nil,
  on_attach = nil,
}

local config = vim.deepcopy(defaults)
local installing = false
local install_callbacks = {}
local last_error

local function notify(message, level)
  vim.notify(message, level or vim.log.levels.INFO, { title = "Composer Support" })
end

local function platform_asset(sysname, machine)
  local systems = {
    Darwin = "apple-darwin",
    Linux = "unknown-linux-musl",
    Windows_NT = "pc-windows-msvc",
  }
  local architectures = {
    x86_64 = "x86_64",
    AMD64 = "x86_64",
    arm64 = "aarch64",
    aarch64 = "aarch64",
    ARM64 = "aarch64",
  }
  local system = systems[sysname]
  local architecture = architectures[machine]
  if not system or not architecture then
    return nil, ("unsupported platform: %s %s"):format(sysname, machine)
  end

  local suffix = sysname == "Windows_NT" and ".exe" or ""
  return ("composer-language-server-%s-%s%s"):format(architecture, system, suffix)
end

local function managed_server()
  local uname = vim.uv.os_uname()
  local asset, err = platform_asset(uname.sysname, uname.machine)
  if not asset then
    return nil, nil, err
  end

  local directory = vim.fs.joinpath(vim.fn.stdpath("data"), "composer-support", "bin")
  local target = asset:gsub("^composer%-language%-server%-", "")
  local path = vim.fs.joinpath(directory, ("composer-language-server-%s-%s"):format(SERVER_VERSION, target))
  return path, asset
end

local function executable(path, minimum_bytes)
  local stat = path and vim.uv.fs_stat(path)
  if not stat or stat.type ~= "file" or stat.size < (minimum_bytes or 1) then
    return false
  end
  return vim.fn.has("win32") == 1 or vim.fn.executable(path) == 1
end

local function configured_server()
  if not config.server_path then
    local path, asset, err = managed_server()
    if not path then
      return nil, nil, err
    end
    if executable(path, MIN_SERVER_BYTES) then
      return path, asset
    end
    return nil, asset, ("composer-language-server %s is not installed"):format(SERVER_VERSION)
  end

  local path = vim.fn.expand(config.server_path)
  if not path:find("[/\\]") then
    local resolved = vim.fn.exepath(path)
    if resolved ~= "" then
      path = resolved
    end
  end
  if executable(path) then
    return path
  end
  return nil, nil, ("configured server is missing or not executable: %s"):format(path)
end

local function fallback_server()
  local current, asset = managed_server()
  if not current then
    return nil
  end
  local directory = vim.fs.dirname(current)
  local directory_stat = vim.uv.fs_stat(directory)
  if not directory_stat or directory_stat.type ~= "directory" then
    return nil
  end

  local target = asset:gsub("^composer%-language%-server%-", "")
  local prefix = "composer-language-server-"
  local suffix = "-" .. target
  local candidates = {}
  for name, kind in vim.fs.dir(directory) do
    if (kind == "file" or kind == "link")
      and name:sub(1, #prefix) == prefix
      and name:sub(-#suffix) == suffix
    then
      local version = name:sub(#prefix + 1, #name - #suffix)
      local major, minor, patch = version:match("^(%d+)%.(%d+)%.(%d+)$")
      local path = vim.fs.joinpath(directory, name)
      if major and executable(path, MIN_SERVER_BYTES) then
        table.insert(candidates, {
          path = path,
          version = { tonumber(major), tonumber(minor), tonumber(patch) },
        })
      end
    end
  end
  table.sort(candidates, function(left, right)
    for index = 1, 3 do
      if left.version[index] ~= right.version[index] then
        return left.version[index] > right.version[index]
      end
    end
    return false
  end)
  local fallback = candidates[1]
  return fallback and fallback.path or nil
end

local function finish_install(path, err)
  installing = false
  local callbacks = install_callbacks
  install_callbacks = {}
  for _, callback in ipairs(callbacks) do
    callback(path, err)
  end
end

local function install_server(callback)
  table.insert(install_callbacks, callback)
  if installing then
    return
  end
  installing = true

  local path, asset, platform_error = managed_server()
  if not path then
    finish_install(nil, platform_error)
    return
  end
  if executable(path, MIN_SERVER_BYTES) then
    finish_install(path)
    return
  end
  if vim.uv.fs_stat(path) then
    local removed, remove_error = vim.uv.fs_unlink(path)
    if not removed then
      finish_install(nil, ("could not replace invalid language server: %s"):format(remove_error))
      return
    end
  end
  if vim.fn.executable("curl") ~= 1 then
    finish_install(nil, "curl is required to download the language server; install it or set server_path")
    return
  end

  local directory = vim.fs.dirname(path)
  local directory_ok, directory_error = pcall(vim.fn.mkdir, directory, "p")
  local directory_stat = vim.uv.fs_stat(directory)
  if not directory_ok or not directory_stat or directory_stat.type ~= "directory" then
    finish_install(nil, ("could not create %s: %s"):format(directory, directory_error))
    return
  end

  local temporary = ("%s.%d.download"):format(path, vim.uv.os_getpid())
  vim.uv.fs_unlink(temporary)
  local url = ("https://github.com/%s/releases/download/v%s/%s"):format(REPOSITORY, SERVER_VERSION, asset)
  local command = {
    "curl",
    "--fail",
    "--location",
    "--silent",
    "--show-error",
    "--connect-timeout",
    "10",
    "--max-time",
    "120",
    "--output",
    temporary,
    url,
  }

  vim.system(command, { text = true }, function(result)
    vim.schedule(function()
      if result.code ~= 0 then
        vim.uv.fs_unlink(temporary)
        local detail = vim.trim(result.stderr or "")
        finish_install(nil, detail ~= "" and detail or ("download failed with exit code %d"):format(result.code))
        return
      end

      local stat = vim.uv.fs_stat(temporary)
      if not stat or stat.type ~= "file" or stat.size < MIN_SERVER_BYTES then
        vim.uv.fs_unlink(temporary)
        finish_install(nil, "downloaded language server is not a valid executable")
        return
      end
      if vim.fn.has("win32") ~= 1 then
        local chmod_ok, chmod_error = vim.uv.fs_chmod(temporary, 493)
        if not chmod_ok then
          vim.uv.fs_unlink(temporary)
          finish_install(nil, ("could not make the language server executable: %s"):format(chmod_error))
          return
        end
      end

      local renamed, rename_error = vim.uv.fs_rename(temporary, path)
      if not renamed then
        vim.uv.fs_unlink(temporary)
        if executable(path, MIN_SERVER_BYTES) then
          finish_install(path)
          return
        end
        finish_install(nil, ("could not install the language server: %s"):format(rename_error))
        return
      end
      finish_install(path)
    end)
  end)
end

local function report_error(err)
  if err and err ~= last_error then
    last_error = err
    notify(err, vim.log.levels.ERROR)
  end
end

local function composer_buffer(bufnr)
  if not vim.api.nvim_buf_is_valid(bufnr) or vim.bo[bufnr].buftype ~= "" then
    return false
  end
  local path = vim.api.nvim_buf_get_name(bufnr)
  return vim.fs.basename(path):lower() == "composer.json"
end

local function enable_buffer_features(client, bufnr)
  if config.inlay_hints and client:supports_method("textDocument/inlayHint") then
    vim.lsp.inlay_hint.enable(true, { bufnr = bufnr })
  end
  if config.open_key then
    vim.keymap.set("n", config.open_key, M.open_package, {
      buffer = bufnr,
      desc = "Open Composer package on Packagist",
      silent = true,
    })
  end
  if type(config.on_attach) == "function" then
    config.on_attach(client, bufnr)
  end
end

local function start_with_server(bufnr, server_path)
  if not composer_buffer(bufnr) then
    return
  end
  last_error = nil
  local document_path = vim.api.nvim_buf_get_name(bufnr)
  local client_id, err = vim.lsp.start({
    name = "composer-language-server",
    cmd = { server_path },
    cmd_env = {
      [CACHE_ENV] = vim.fs.joinpath(vim.fn.stdpath("cache"), "composer-language-server"),
    },
    root_dir = vim.fs.dirname(document_path),
    init_options = {
      check_updates = config.check_updates,
    },
    on_attach = enable_buffer_features,
  }, { bufnr = bufnr })
  if not client_id then
    report_error(err or "could not start composer-language-server")
  end
end

function M.start(bufnr)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  if not composer_buffer(bufnr) then
    return
  end

  local path, _, err = configured_server()
  if path then
    start_with_server(bufnr, path)
    return
  end
  if config.server_path or not config.auto_install then
    if not config.server_path then
      local fallback = fallback_server()
      if fallback then
        notify(("Using cached language server %s"):format(fallback), vim.log.levels.WARN)
        start_with_server(bufnr, fallback)
        return
      end
    end
    report_error(err or "language server is not installed; run :ComposerSupportInstall")
    return
  end

  install_server(function(installed_path, install_error)
    if not installed_path then
      local fallback = fallback_server()
      if fallback then
        notify(("%s\nUsing cached language server %s"):format(install_error, fallback), vim.log.levels.WARN)
        start_with_server(bufnr, fallback)
        return
      end
      report_error(install_error)
      return
    end
    start_with_server(bufnr, installed_path)
  end)
end

local function position_in_range(line, character, range)
  local starts_before = line > range.start.line
    or (line == range.start.line and character >= range.start.character)
  local ends_after = line < range["end"].line
    or (line == range["end"].line and character < range["end"].character)
  return starts_before and ends_after
end

function M.open_package()
  local bufnr = vim.api.nvim_get_current_buf()
  local cursor = vim.api.nvim_win_get_cursor(0)
  local params = { textDocument = vim.lsp.util.make_text_document_params(bufnr) }
  vim.lsp.buf_request_all(bufnr, "textDocument/documentLink", params, function(results)
    for _, response in pairs(results or {}) do
      for _, link in ipairs(response.result or {}) do
        if link.target and position_in_range(cursor[1] - 1, cursor[2], link.range) then
          local _, open_error = vim.ui.open(link.target)
          if open_error then
            report_error(open_error)
          end
          return
        end
      end
    end
    notify("No Composer package under the cursor", vim.log.levels.WARN)
  end)
end

function M.install()
  install_server(function(path, err)
    if path then
      last_error = nil
      notify(("Installed composer-language-server %s"):format(SERVER_VERSION))
    else
      report_error(err)
    end
  end)
end

function M.info()
  local path, _, err = configured_server()
  if path then
    notify(("composer-language-server %s\n%s"):format(SERVER_VERSION, path))
  else
    local fallback = not config.server_path and fallback_server() or nil
    if fallback then
      notify(("composer-language-server %s is not installed\nCached fallback: %s"):format(SERVER_VERSION, fallback), vim.log.levels.WARN)
    else
      notify(err or ("composer-language-server %s is not installed"):format(SERVER_VERSION), vim.log.levels.WARN)
    end
  end
end

function M.setup(options)
  if vim.fn.has("nvim-0.10") ~= 1 then
    notify("Neovim 0.10 or newer is required", vim.log.levels.ERROR)
    return
  end
  options = options or {}
  if type(options) ~= "table" then
    notify("setup options must be a table", vim.log.levels.ERROR)
    return
  end
  local next_config = vim.tbl_deep_extend("force", vim.deepcopy(defaults), options)
  if next_config.server_path ~= nil and type(next_config.server_path) ~= "string" then
    notify("server_path must be a string", vim.log.levels.ERROR)
    return
  end
  if next_config.open_key ~= false and type(next_config.open_key) ~= "string" then
    notify("open_key must be a string or false", vim.log.levels.ERROR)
    return
  end
  for _, option in ipairs({ "auto_install", "check_updates", "inlay_hints" }) do
    if type(next_config[option]) ~= "boolean" then
      notify(("%s must be a boolean"):format(option), vim.log.levels.ERROR)
      return
    end
  end
  if next_config.on_attach ~= nil and type(next_config.on_attach) ~= "function" then
    notify("on_attach must be a function", vim.log.levels.ERROR)
    return
  end
  config = next_config

  vim.api.nvim_create_user_command("ComposerSupportInstall", M.install, {
    desc = "Install the Composer language server",
    force = true,
  })
  vim.api.nvim_create_user_command("ComposerSupportInfo", M.info, {
    desc = "Show Composer language server information",
    force = true,
  })
  vim.api.nvim_create_user_command("ComposerOpenPackage", M.open_package, {
    desc = "Open the Composer package under the cursor",
    force = true,
  })

  local group = vim.api.nvim_create_augroup("ComposerSupport", { clear = true })
  vim.api.nvim_create_autocmd({ "BufReadPost", "BufNewFile" }, {
    group = group,
    pattern = "composer.json",
    callback = function(args)
      M.start(args.buf)
    end,
  })
  vim.schedule(function()
    for _, bufnr in ipairs(vim.api.nvim_list_bufs()) do
      if vim.api.nvim_buf_is_loaded(bufnr) and composer_buffer(bufnr) then
        M.start(bufnr)
      end
    end
  end)
end

M._platform_asset = platform_asset
M._server_version = SERVER_VERSION

return M
