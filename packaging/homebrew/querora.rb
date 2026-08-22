cask "querora" do
  version "0.1.0"
  sha256 :no_check # pinned on first stable release

  # github.com/8ugustdev/querora releases (URL live from the first tagged release)
  url "https://github.com/8ugustdev/querora/releases/download/v#{version}/Querora_#{version}_aarch64.dmg"
  name "Querora"
  desc "Local-first conversational BI for macOS — the AI brain is your CLI agent"
  homepage "https://github.com/8ugustdev/querora"

  app "Querora.app"

  # The querora-mcp shim ships inside the app bundle (Contents/MacOS) and is
  # found automatically; no postflight linking needed.
  zap trash: [
    "~/.querora",
    "~/Library/Application Support/dev.querora.app",
    "~/Library/Caches/dev.querora.app",
  ]
end
