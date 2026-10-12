// The marketing site's one script: motion and the few controls that need it.
// Every page reads in full without it. No data, no requests, no storage.
(function () {
  'use strict';
  window.shoninSite = true;

  var root = document.documentElement;
  var motion = root.classList.contains('motion');
  var reduce = window.matchMedia('(prefers-reduced-motion: reduce)');

  // Scroll reveals (M2) and the choreographed sections (M4, M11, M12): each
  // [data-play] scope plays once, when a fifth of it is in view (or a fifth
  // of the screen, for scopes taller than five screens).
  function reveal() {
    var scopes = Array.prototype.slice.call(document.querySelectorAll('[data-play]:not(.is-in)'));
    if (!motion) {
      scopes.forEach(function (scope) { scope.classList.add('is-in'); });
      return;
    }
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        var tall = entry.intersectionRect.height >= window.innerHeight * 0.2;
        if (entry.intersectionRatio >= 0.2 || tall) {
          entry.target.classList.add('is-in');
          io.unobserve(entry.target);
        }
      });
    }, { threshold: [0, 0.05, 0.1, 0.15, 0.2] });
    scopes.forEach(function (scope) { io.observe(scope); });
  }

  // Ambient loops (M1, the money pulses, the drifting card) run only while
  // their scope is on screen and the tab is visible.
  function loops() {
    if (!motion) return;
    var scopes = Array.prototype.slice.call(document.querySelectorAll('[data-loop]'));
    var seen = new Map();
    function sync(scope) {
      scope.classList.toggle('is-paused', !seen.get(scope) || document.hidden);
    }
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        seen.set(entry.target, entry.isIntersecting);
        sync(entry.target);
      });
    });
    scopes.forEach(function (scope) {
      seen.set(scope, true);
      io.observe(scope);
    });
    document.addEventListener('visibilitychange', function () { scopes.forEach(sync); });
  }

  // The phone and tablet menu: a modal dialog. Focus moves in and stays in;
  // Esc and Close return it to "Open menu".
  function menu() {
    var dialog = document.getElementById('menu');
    var opener = document.querySelector('.site-header__menu');
    var page = document.getElementById('page');
    if (!dialog || !opener || !page) return;
    var closer = dialog.querySelector('.menu__close');
    opener.hidden = false;

    function focusable() {
      return Array.prototype.slice
        .call(dialog.querySelectorAll('a[href], button:not([disabled])'))
        .filter(function (el) { return el.getClientRects().length > 0; });
    }
    function open() {
      dialog.hidden = false;
      page.inert = true;
      root.classList.add('menu-open');
      opener.setAttribute('aria-expanded', 'true');
      closer.focus();
    }
    function close(restore) {
      if (dialog.hidden) return;
      dialog.hidden = true;
      page.inert = false;
      root.classList.remove('menu-open');
      opener.setAttribute('aria-expanded', 'false');
      if (restore) opener.focus();
    }
    opener.addEventListener('click', open);
    closer.addEventListener('click', function () { close(true); });
    dialog.addEventListener('keydown', function (event) {
      if (event.key === 'Escape') {
        event.preventDefault();
        close(true);
        return;
      }
      if (event.key !== 'Tab') return;
      var items = focusable();
      var first = items[0];
      var last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    });
    dialog.addEventListener('click', function (event) {
      if (event.target.closest('a[href]')) close(false);
    });
    window.matchMedia('(min-width: 1024px)').addEventListener('change', function (query) {
      if (query.matches) close(false);
    });
  }

  // The questions on a phone: an accordion, one answer open at a time.
  function accordions() {
    document.querySelectorAll('[data-accordion]').forEach(function (group) {
      var buttons = Array.prototype.slice.call(group.querySelectorAll('[aria-controls]'));
      function set(button, open, animate) {
        var panel = document.getElementById(button.getAttribute('aria-controls'));
        button.setAttribute('aria-expanded', open ? 'true' : 'false');
        if (!panel) return;
        panel.hidden = !open;
        if (open && animate) {
          panel.classList.remove('hx-panel');
          void panel.offsetWidth;
          panel.classList.add('hx-panel');
        }
      }
      buttons.forEach(function (button, index) { set(button, index === 0, false); });
      buttons.forEach(function (button) {
        button.addEventListener('click', function () {
          var wasOpen = button.getAttribute('aria-expanded') === 'true';
          buttons.forEach(function (other) { set(other, false, false); });
          if (!wasOpen) set(button, true, true);
        });
      });
    });
  }

  // "Publishing is easy" (M3). On a phone the steps are a snap-scroll row
  // with the visual for the snapped card below; on a desktop the visual is
  // pinned and swaps as each step crosses the middle of the screen. A tablet,
  // and reduced motion on a desktop, keep the drawn state.
  //
  // On a desktop the active step is the last one whose top is above the
  // middle of the screen, read from the layout once a frame while scrolling.
  // That is a pure function of the scroll position, so each crossing changes
  // the step exactly once, however fast the reader scrolls (a zero-height
  // observer band misses a step that jumps past it, and fires again for any
  // step that moves).
  function publishing() {
    var section = document.querySelector('[data-steps]');
    if (!section) return;
    var steps = Array.prototype.slice.call(section.querySelectorAll('.pstep'));
    var list = section.querySelector('.publish__list');
    var controls = section.querySelector('.publish__controls');
    var count = section.querySelector('[data-step-now]');
    var dots = Array.prototype.slice.call(section.querySelectorAll('.publish__dots span'));
    var phone = window.matchMedia('(max-width: 599px)');
    var desktop = window.matchMedia('(min-width: 1024px)');
    var observer = null;
    var frame = 0;

    function pick() {
      frame = 0;
      var line = window.innerHeight / 2;
      var n = 1;
      steps.forEach(function (step, index) {
        if (step.getBoundingClientRect().top <= line) n = index + 1;
      });
      if (section.getAttribute('data-active') !== String(n)) show(n);
    }
    function schedule() {
      if (!frame) frame = window.requestAnimationFrame(pick);
    }
    function follow(on) {
      var method = on ? 'addEventListener' : 'removeEventListener';
      window[method]('scroll', schedule, { passive: true });
      window[method]('resize', schedule, { passive: true });
      if (!on && frame) {
        window.cancelAnimationFrame(frame);
        frame = 0;
      }
    }

    function show(n) {
      section.setAttribute('data-active', String(n));
      steps.forEach(function (step, index) {
        step.classList.toggle('is-done', index + 1 < n);
        step.classList.toggle('is-active', index + 1 === n);
        if (index + 1 === n) step.setAttribute('aria-current', 'step');
        else step.removeAttribute('aria-current');
      });
      dots.forEach(function (dot, index) {
        dot.classList.toggle('is-done', index + 1 < n);
        dot.classList.toggle('is-active', index + 1 === n);
      });
      if (count && count.textContent !== String(n)) count.textContent = String(n);
    }
    function mode() {
      if (observer) observer.disconnect();
      observer = null;
      follow(false);
      var observe = 'IntersectionObserver' in window;
      var carousel = phone.matches;
      var scrolly = desktop.matches && motion && !reduce.matches && observe;
      section.classList.toggle('is-stepped', carousel || scrolly);
      controls.hidden = !carousel;
      if (carousel && !observe) {
        show(1);
      } else if (carousel) {
        observer = new IntersectionObserver(function (entries) {
          entries.forEach(function (entry) {
            if (entry.isIntersecting) show(steps.indexOf(entry.target) + 1);
          });
        }, { root: list, threshold: 0.6 });
        steps.forEach(function (step) { observer.observe(step); });
        show(1);
      } else if (scrolly) {
        follow(true);
        pick();
      } else {
        show(2);
      }
    }
    function go(delta) {
      var n = Number(section.getAttribute('data-active') || '1') + delta;
      n = Math.max(1, Math.min(steps.length, n));
      var step = steps[n - 1];
      list.scrollTo({
        left: step.offsetLeft - list.offsetLeft - parseFloat(getComputedStyle(list).paddingLeft),
        behavior: reduce.matches ? 'auto' : 'smooth'
      });
      show(n);
    }
    // Without an observer, follow the row's own scrolling.
    list.addEventListener('scroll', function () {
      if (!phone.matches || 'IntersectionObserver' in window) return;
      var first = steps[0].getBoundingClientRect().left;
      var stride = steps.length > 1 ? steps[1].getBoundingClientRect().left - first : 1;
      var n = Math.round((list.getBoundingClientRect().left - first) / stride) + 1;
      show(Math.max(1, Math.min(steps.length, n)));
    }, { passive: true });
    section.querySelector('[data-step-prev]').addEventListener('click', function () { go(-1); });
    section.querySelector('[data-step-next]').addEventListener('click', function () { go(1); });
    phone.addEventListener('change', mode);
    desktop.addEventListener('change', mode);
    reduce.addEventListener('change', mode);
    mode();
  }

  // The template's four commands, copied without their prompts.
  function copyButtons() {
    if (!navigator.clipboard) return;
    document.querySelectorAll('[data-copy]').forEach(function (button) {
      var source = document.getElementById(button.getAttribute('data-copy'));
      if (!source) return;
      var status = button.closest('figure') && button.closest('figure').querySelector('[aria-live]');
      button.hidden = false;
      button.addEventListener('click', function () {
        var text = source.textContent.split('\n').map(function (line) {
          return line.replace(/^\$ /, '');
        }).join('\n');
        navigator.clipboard.writeText(text).then(function () {
          button.classList.add('is-copied');
          if (status) status.textContent = 'Copied the four commands.';
          setTimeout(function () {
            button.classList.remove('is-copied');
            if (status) status.textContent = '';
          }, 1500);
        });
      });
    });
  }

  reveal();
  loops();
  menu();
  accordions();
  publishing();
  copyButtons();
})();
