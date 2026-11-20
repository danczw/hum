# Template for Formula/hum.rb in danczw/homebrew-tap, filled in by
# .github/workflows/release.yml.
class Hum < Formula
  desc "Terminal monitor for machines running coding agents"
  homepage "https://github.com/danczw/hum"
  version "@VERSION@"
  license "Apache-2.0"

  depends_on :linux

  on_intel do
    url "https://github.com/danczw/hum/releases/download/v@VERSION@/hum-x86_64-unknown-linux-musl.tar.gz"
    sha256 "@SHA256_X86_64@"
  end

  on_arm do
    url "https://github.com/danczw/hum/releases/download/v@VERSION@/hum-aarch64-unknown-linux-musl.tar.gz"
    sha256 "@SHA256_AARCH64@"
  end

  def install
    bin.install "hum"
  end

  test do
    assert_equal "hum #{version}", shell_output("#{bin}/hum --version").strip
  end
end
