# Homebrew formula for sinter. Lives in the shellfu/homebrew-tap repo as
# Formula/sinter.rb; this copy is the source of truth. Placeholder sha256s
# are rewritten by update-formula.sh for a given release version.
class Sinter < Formula
  desc "Code graph for agents: typed nodes/edges with evidence-based resolution"
  homepage "https://github.com/shellfu/sinter"
  version "0.38.0"
  license "MIT OR Apache-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/shellfu/sinter/releases/download/v#{version}/sinter-aarch64-apple-darwin.tar.gz"
      sha256 "1a8a7be5f86a3de974c03a488fa87e34e005421a11b1460ec520ee24cb7aff26"
    else
      url "https://github.com/shellfu/sinter/releases/download/v#{version}/sinter-x86_64-apple-darwin.tar.gz"
      sha256 "e070e85970fcdfff9cee76aa07e630846ffc414fdea7286885c6cfe5aac9c120"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/shellfu/sinter/releases/download/v#{version}/sinter-aarch64-unknown-linux-musl.tar.gz"
      sha256 "26bccc1aa88dca9d64b7b807d282ddc436ad60269e01e4393f4e25768b183382"
    else
      url "https://github.com/shellfu/sinter/releases/download/v#{version}/sinter-x86_64-unknown-linux-musl.tar.gz"
      sha256 "6f58c2488f4aee92bfb6a87c996f7e2a7be5c94c19352dbadc57b3f28c210dec"
    end
  end

  def install
    bin.install "sinter"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/sinter --version")
  end
end
