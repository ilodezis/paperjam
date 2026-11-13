// Drawer: guide tabs pull their card up; the previous one sinks back.
const drawer = document.querySelector("[data-drawer]");
if (drawer) {
  const tabs = [...drawer.querySelectorAll('[role="tab"]')];
  const select = (tab) => {
    const current = drawer.querySelector(".cat-card.is-up");
    const next = document.getElementById(tab.getAttribute("aria-controls"));
    if (!next || next === current) return;
    tabs.forEach((t) => {
      const on = t === tab;
      t.setAttribute("aria-selected", String(on));
      t.tabIndex = on ? 0 : -1;
    });
    if (current) {
      current.classList.remove("is-up");
      current.classList.add("is-sinking");
      current.hidden = true;
      window.setTimeout(() => current.classList.remove("is-sinking"), 450);
    }
    next.hidden = false;
    drawer.dataset.active = next.id.replace("card-", "");
    // next frame, so the rise transitions from the sunk position
    requestAnimationFrame(() => requestAnimationFrame(() => next.classList.add("is-up")));
  };
  tabs.forEach((tab, i) => {
    tab.addEventListener("click", () => select(tab));
    tab.addEventListener("keydown", (e) => {
      const step = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
      if (!step) return;
      e.preventDefault();
      const target = tabs[(i + step + tabs.length) % tabs.length];
      target.focus();
      select(target);
    });
  });
}

// Reel: one quiet play button over the poster; native controls come back once it plays.
const reel = document.querySelector("[data-reel]");
if (reel) {
  const video = reel.querySelector("video");
  const play = reel.querySelector(".reel-play");
  const start = () => {
    play.hidden = true;
    video.controls = true;
    video.play().catch(() => {});
    video.focus({ preventScroll: true });
  };
  video.controls = false;
  play.hidden = false;
  play.addEventListener("click", start);
  video.addEventListener("play", () => { play.hidden = true; video.controls = true; });

  // The hero link scrolls down to the reel and starts it: the click is the consent to sound.
  document.querySelectorAll("[data-play-tour]").forEach((link) => {
    link.addEventListener("click", (e) => {
      e.preventDefault();
      reel.scrollIntoView({ behavior: "smooth", block: "center" });
      start();
    });
  });

  // A soft ring on the play button, once, the first time the reel comes into view.
  const pill = play.querySelector(".reel-pill");
  if ("IntersectionObserver" in window) {
    const seen = new IntersectionObserver((entries) => {
      if (!entries.some((e) => e.isIntersecting)) return;
      seen.disconnect();
      window.setTimeout(() => pill.classList.add("is-calling"), 500);
    }, { threshold: 0.6 });
    seen.observe(reel);
  }
}

// Copy buttons
const status = document.getElementById("copy-status");
let resetTimer;

async function copyText(text) {
  if (navigator.clipboard && window.isSecureContext) {
    await navigator.clipboard.writeText(text);
    return;
  }
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.append(area);
  area.select();
  const ok = document.execCommand("copy");
  area.remove();
  if (!ok) throw new Error("copy failed");
}

document.querySelectorAll("[data-copy]").forEach((button) => {
  const label = button.querySelector("span");
  button.addEventListener("click", async () => {
    const source = document.getElementById(button.dataset.copy);
    if (!source) return;
    try {
      await copyText(source.textContent.trim());
      button.classList.add("copied");
      label.textContent = "Copied";
      status.textContent = "Command copied.";
    } catch {
      label.textContent = "Select it";
      status.textContent = "Couldn’t copy automatically. Select the command and copy it by hand.";
    }
    window.clearTimeout(resetTimer);
    resetTimer = window.setTimeout(() => {
      document.querySelectorAll("[data-copy]").forEach((b) => {
        b.classList.remove("copied");
        b.querySelector("span").textContent = "Copy";
      });
    }, 2200);
  });
});
