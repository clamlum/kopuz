final: prev: {
  yt-dlp = prev.yt-dlp.overridePythonAttrs (oldAttrs: rec {
    version = "2026.08.19";
    src = prev.fetchFromGitHub {
      owner = "yt-dlp";
      repo = "yt-dlp";
      rev = version;
      hash = "sha256-BM5ZeGTmHq+1xH6G/zsuCtjLgYgfRA11ya0zIHK5p4g=";
    };
  });
}
