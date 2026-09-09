cask "pix" do
  version "0.1.6"
  sha256 "987bd3777130227d6e2ef57432b196315aba1619af5a12051ead3bac68b1d333"

  url "https://github.com/ZainCheung/pix/releases/download/v#{version}/pix-#{version}-macos-arm64.zip"
  name "Pix"
  desc "Secure menu-bar host for Pi"
  homepage "https://github.com/ZainCheung/pix"

  auto_updates true
  depends_on arch: :arm64
  depends_on macos: :sonoma

  app "Pix.app"
  # Expose the CLI embedded in Pix.app; this is a launcher for the same
  # canonical binary the menu-bar app uses, not a second CLI installation.
  binary "#{appdir}/Pix.app/Contents/Resources/pix", target: "pix"

  # Unload the user LaunchAgent and quit the menu-bar app, but preserve the
  # Host configuration, Keychain identity, authorized workspaces, and Pi
  # native session files.
  uninstall launchctl: "com.deepoke.pix.host",
            quit:      "com.pix.macos"
end
