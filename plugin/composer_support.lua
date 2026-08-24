if vim.g.loaded_composer_support == 1 then
  return
end
vim.g.loaded_composer_support = 1

require("composer_support").setup()
