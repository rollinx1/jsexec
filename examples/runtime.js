const runtime = {};
runtime.p = "/static/";
runtime.u = (id) =>
  "js/" + ({ 7: "login" }[id] || id) + "-" + { 7: "abc123", 9: "def456" }[id] + ".chunk.js";

const __vite__mapDeps = (
  indexes,
  map = __vite__mapDeps,
  dependencies = map.f || (map.f = ["assets/settings-AbCdEf.js", "assets/theme.css"])
) => indexes.map((index) => dependencies[index]);

import("./profile.js");
