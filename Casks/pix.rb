cask "pix" do
  version "0.1.0"
  sha256 "68ccdf9f6b3776ab6dda37485efc16c3bfef6c50e16e4088c8722225699449bc"

  url "https://github.com/ZainCheung/pix/releases/download/v#{version}/pix-#{version}-macos-arm64.zip"
  name "Pix"
  desc "Secure menu-bar host for Pi"
  homepage "https://github.com/ZainCheung/pix"

  depends_on arch: :arm64
  depends_on macos: :sonoma

  app "Pix.app"
  binary "#{appdir}/Pix.app/Contents/Resources/pix", target: "pix"

  # Unload the user LaunchAgent and quit the menu-bar app, but preserve the
  # Host configuration, Keychain identity, authorized workspaces, and Pi
  # native session files.
  uninstall launchctl: "com.deepoke.pix.host",
            quit:      "com.pix.macos"
end
