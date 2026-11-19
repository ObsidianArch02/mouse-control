class MouseControl < Formula
  desc "macOS menu bar utility for scroll direction, unified scroll travel, and side button remapping"
  homepage "https://github.com/ObsidianArch02/mouse-control"
  url "https://github.com/ObsidianArch02/mouse-control/releases/download/v0.1.0/mouse-control-v0.1.0-darwin-arm64.tar.gz"
  sha256 "REPLACE_WITH_RELEASE_TARBALL_SHA256"
  license "MIT"
  version "0.1.0"

  depends_on :macos
  depends_on arch: :arm64

  def install
    bin.install "mouse-control"
    pkgshare.install "config.toml"
  end

  def post_install
    identity = ENV.fetch("MOUSE_CONTROL_CODESIGN_IDENTITY", "mouse-control-codesign")
    identities = Utils.popen_read("/usr/bin/security", "find-identity", "-v", "-p", "codesigning")
    return unless identities.include?(%("#{identity}"))

    quiet_system "/usr/bin/codesign", "--force", "--sign", identity,
                 "--identifier", "com.eacryo.mouse-control", (bin/"mouse-control").to_s
  end

  service do
    run [opt_bin/"mouse-control", "run", "--log-stdout"]
    keep_alive crashed: true
    process_type :interactive
    log_path var/"log/mouse-control.log"
    error_log_path var/"log/mouse-control.error.log"
  end

  def caveats
    <<~EOS
      Next steps:
        1. (Recommended once) Create a local self-signed Code Signing certificate named
           "mouse-control-codesign" in Keychain Access (Self-Signed Root, Code Signing,
           Trust -> Code Signing: Always Trust) and sign the binary so Accessibility
           permission persists across upgrades:
             mouse-control sign
        2. Start the background service in your user GUI session (do not use sudo):
             brew services start mouse-control
        3. Grant Accessibility permission under:
             System Settings -> Privacy & Security -> Accessibility
        4. Verify status:
             mouse-control status

      Configuration (XDG):
        $XDG_CONFIG_HOME/mouse-control/config.toml
        (defaults to ~/.config/mouse-control/config.toml)
      Example config:
        #{opt_pkgshare}/config.toml
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/mouse-control --version")
  end
end
