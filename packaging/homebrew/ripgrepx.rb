# Homebrew formula for the `rgx` command. Copy this into your tap (igorgatis/homebrew-ripgrepx) as
# Formula/ripgrepx.rb to bootstrap it; the release workflow regenerates it (version + sha256) on each
# tag. Install with: brew install igorgatis/ripgrepx/ripgrepx
class Ripgrepx < Formula
  desc "Instant ripgrep via a persistent index (the rgx command)"
  homepage "https://github.com/igorgatis/ripgrepx"
  version "0.1.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/igorgatis/ripgrepx/releases/download/v0.1.0/rgx-v0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "30b8860862be1af22678c67db9062a7fc02c9a1ce574df6328a76d78eee0d472"
    end
    on_intel do
      url "https://github.com/igorgatis/ripgrepx/releases/download/v0.1.0/rgx-v0.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "a47ec074960baaab7dd05a64d31720984cde717f553c09fcf76c89f59b7e9839"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/igorgatis/ripgrepx/releases/download/v0.1.0/rgx-v0.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "5e00fbbf9f3ded4ce679ec1f1b3cd06bacfdca9baedc7c85388dd5ba0f0f4adf"
    end
    on_intel do
      url "https://github.com/igorgatis/ripgrepx/releases/download/v0.1.0/rgx-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "fb37db9880ee81244c60ef0f5a3f81bb37f5f4e517b7be321ac433736532863d"
    end
  end

  def install
    bin.install "rgx"
  end

  test do
    assert_match "usage", shell_output("#{bin}/rgx --help 2>&1", 2)
  end
end
