/* Surform docs — progressive enhancements: theme toggle, copy buttons,
   search, prev/next pager, TOC scroll-spy, mobile nav. Everything
   degrades: without JS the pages read perfectly. */
(function () {
  "use strict";

  /* ---------- theme ---------- */
  var toggle = document.getElementById("theme-toggle");
  if (toggle) {
    toggle.addEventListener("click", function () {
      var root = document.documentElement;
      var dark = root.dataset.theme !== "light";
      root.dataset.theme = dark ? "light" : "dark";
      localStorage.setItem("surform-theme", dark ? "light" : "dark");
    });
  }

  /* ---------- mobile nav ---------- */
  var navtoggle = document.getElementById("navtoggle");
  var sidebar = document.getElementById("sidebar");
  if (navtoggle && sidebar) {
    navtoggle.addEventListener("click", function () {
      sidebar.classList.toggle("open");
    });
    document.addEventListener("click", function (e) {
      if (sidebar.classList.contains("open") &&
          !sidebar.contains(e.target) && e.target !== navtoggle) {
        sidebar.classList.remove("open");
      }
    });
  }

  /* ---------- copy buttons ---------- */
  document.querySelectorAll(".codebox").forEach(function (box) {
    var pre = box.querySelector("pre code");
    var btn = box.querySelector(".copy");
    if (!pre || !btn) return;
    btn.addEventListener("click", function () {
      navigator.clipboard.writeText(pre.textContent).then(function () {
        btn.textContent = "Copied";
        btn.classList.add("done");
        setTimeout(function () {
          btn.textContent = "Copy";
          btn.classList.remove("done");
        }, 1400);
      });
    });
  });

  /* ---------- prev/next pager (built from the sidebar order) ---------- */
  var links = Array.prototype.slice.call(
    document.querySelectorAll(".sidebar a"));
  var current = document.querySelector(".sidebar a.active");
  var pager = document.getElementById("pager");
  if (pager && current && links.length) {
    var idx = links.indexOf(current);
    var html = "";
    if (idx > 0) {
      html += "<a class=\"prev\" href=\"" + links[idx - 1].getAttribute("href") +
        "\"><span class=\"dir\">← Previous</span><span class=\"label\">" +
        links[idx - 1].textContent + "</span></a>";
    }
    if (idx + 1 < links.length) {
      html += "<a class=\"next\" href=\"" + links[idx + 1].getAttribute("href") +
        "\"><span class=\"dir\">Next →</span><span class=\"label\">" +
        links[idx + 1].textContent + "</span></a>";
    }
    pager.innerHTML = html;
  }

  /* ---------- toc scroll-spy ---------- */
  var tocLinks = Array.prototype.slice.call(document.querySelectorAll(".toc a"));
  if (tocLinks.length && "IntersectionObserver" in window) {
    var byId = {};
    tocLinks.forEach(function (a) {
      byId[a.getAttribute("href").slice(1)] = a;
    });
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          tocLinks.forEach(function (a) { a.classList.remove("active"); });
          var link = byId[entry.target.id];
          if (link) link.classList.add("active");
        }
      });
    }, { rootMargin: "-56px 0px -70% 0px" });
    Object.keys(byId).forEach(function (id) {
      var el = document.getElementById(id);
      if (el) observer.observe(el);
    });
  }

  /* ---------- search ---------- */
  var input = document.getElementById("search-input");
  var results = document.getElementById("search-results");
  if (!input || !results) return;

  var INDEX = window.SURFORM_SEARCH || [];
  var selected = -1;
  var items = [];

  function score(entry, terms) {
    var total = 0;
    for (var i = 0; i < terms.length; i++) {
      var t = terms[i].toLowerCase();
      var inTitle = entry.t.toLowerCase().indexOf(t);
      var inBody = entry.b.toLowerCase().indexOf(t);
      if (inTitle >= 0) total += 40;
      else if (inBody < 0) return 0;
      if (inBody >= 0) total += 6;
      // word-start bonus in the body
      if (inBody === 0 || entry.b.charAt(inBody - 1) === " ") total += 4;
    }
    return total;
  }

  function snippet(entry, terms) {
    var lower = entry.b.toLowerCase();
    for (var i = 0; i < terms.length; i++) {
      var at = lower.indexOf(terms[i].toLowerCase());
      if (at >= 0) {
        var start = Math.max(0, at - 40);
        return (start > 0 ? "…" : "") + entry.b.slice(start, at + 80) + "…";
      }
    }
    return entry.b.slice(0, 110);
  }

  function render() {
    var q = input.value.trim();
    if (q.length < 2) { results.hidden = true; return; }
    var terms = q.split(/\s+/);
    var hits = [];
    for (var i = 0; i < INDEX.length; i++) {
      var s = score(INDEX[i], terms);
      if (s > 0) hits.push([s, INDEX[i]]);
    }
    hits.sort(function (a, b) { return b[0] - a[0]; });
    hits = hits.slice(0, 8);
    selected = -1;
    items = [];
    if (!hits.length) {
      results.innerHTML = "<div class=\"search-empty\">Nothing matches “" +
        q.replace(/</g, "&lt;") + "”.</div>";
    } else {
      var html = "";
      hits.forEach(function (hit) {
        var e = hit[1];
        html += "<a href=\"" + e.u + "\"><div class=\"r-title\">" +
          e.t.replace(/</g, "&lt;") + "</div><div class=\"r-body\">" +
          snippet(e, terms).replace(/</g, "&lt;") + "</div></a>";
      });
      results.innerHTML = html;
      items = Array.prototype.slice.call(results.querySelectorAll("a"));
    }
    results.hidden = false;
  }

  input.addEventListener("input", render);
  input.addEventListener("focus", render);
  document.addEventListener("keydown", function (e) {
    // "/" focuses the search box from anywhere — except while typing
    // in an input or a contenteditable (the playground's editor).
    if (e.key === "/" && document.activeElement !== input &&
        !/^(INPUT|TEXTAREA)$/.test(document.activeElement.tagName) &&
        !document.activeElement.isContentEditable) {
      e.preventDefault();
      input.focus();
      return;
    }
    if (results.hidden) return;
    if (e.key === "Escape") { results.hidden = true; input.blur(); }
    if (e.key === "ArrowDown" && items.length) {
      e.preventDefault();
      selected = (selected + 1) % items.length;
    } else if (e.key === "ArrowUp" && items.length) {
      e.preventDefault();
      selected = (selected - 1 + items.length) % items.length;
    } else if (e.key === "Enter" && selected >= 0) {
      items[selected].click();
    } else {
      return;
    }
    items.forEach(function (a, i) { a.classList.toggle("sel", i === selected); });
  });
  document.addEventListener("click", function (e) {
    if (!results.hidden && !results.contains(e.target) && e.target !== input) {
      results.hidden = true;
    }
  });
})();
