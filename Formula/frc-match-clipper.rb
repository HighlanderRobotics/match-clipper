class FrcMatchClipper < Formula
  desc "Clip FRC livestreams into match videos with resumable YouTube uploads"
  homepage "https://github.com/jackshim415/match-clipper"
  url "https://github.com/jackshim415/match-clipper.git",
      tag: "v0.2.2",
      revision: "c75b50f52d9c801879be1bff6d4b9f82d07578d5"
  version "0.2.2"

  depends_on "rust" => :build
  depends_on "ffmpeg"
  depends_on "node"
  depends_on "yt-dlp"

  def install
    ENV.prepend_path "PATH", Formula["rust"].opt_bin
    system "cargo", "install", *std_cargo_args
    pkgshare.install ".env.example" => "env.example"
    doc.install "README.md", "AGENTS.md"
  end

  def caveats
    <<~EOS
      Run `frc-match-clipper config-path` to locate your configuration directory.
      Add .env with TBA_API_KEY and YOUTUBE_API_KEY, plus client_secret.json for uploads.
      Use --dry-run to review timing, then --no-upload to verify a local sample.
    EOS
  end

  test do
    assert_match "frc-match-clipper 0.2.2", shell_output("#{bin}/frc-match-clipper --version")
    assert_match "FRC Match Clipper", shell_output("#{bin}/frc-match-clipper --help")
  end
end
