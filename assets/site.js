/* No dependencies, no API calls: a small, interruptible product preview. */
(() => {
  'use strict';
  const $ = (selector) => document.querySelector(selector);
  const $$ = (selector) => [...document.querySelectorAll(selector)];
  const root = document.documentElement;
  root.classList.add('has-script');
  const systemMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
  const motionButton = $('#motion-toggle');
  let userMotion = true;
  try { userMotion = localStorage.getItem('deeptui-motion') !== 'off'; } catch { /* Storage is optional. */ }
  let motion = userMotion && !systemMotion.matches;

  // Navigation remains a normal set of links when the menu is expanded.
  const nav = $('#nav');
  const menu = $('.menu-toggle');
  const links = $('#nav-links');
  const setMenu = (open) => {
    menu.setAttribute('aria-expanded', String(open));
    menu.setAttribute('aria-label', open ? 'Close navigation' : 'Open navigation');
    links.classList.toggle('is-open', open);
  };
  menu.addEventListener('click', () => setMenu(menu.getAttribute('aria-expanded') !== 'true'));
  links.addEventListener('click', (event) => { if (event.target.closest('a')) setMenu(false); });
  document.addEventListener('click', (event) => { if (!nav.contains(event.target)) setMenu(false); });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && menu.getAttribute('aria-expanded') === 'true') {
      setMenu(false);
      menu.focus();
    }
  });
  const navState = () => nav.classList.toggle('scrolled', window.scrollY > 12);
  window.addEventListener('scroll', navState, { passive: true });
  navState();

  // Progressive reveals: the page is fully visible if observers are unavailable.
  const revealItems = $$('[data-reveal]');
  if ('IntersectionObserver' in window) {
    const groups = new WeakMap();
    const revealObserver = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          entry.target.classList.add('is-visible');
          revealObserver.unobserve(entry.target);
        }
      }
    }, { threshold: 0.05, rootMargin: '0px 0px -24px 0px' });
    for (const item of revealItems) {
      const index = groups.get(item.parentElement) || 0;
      groups.set(item.parentElement, index + 1);
      item.style.setProperty('--delay', `${Math.min(index, 2) * 70}ms`);
      revealObserver.observe(item);
    }
  } else {
    revealItems.forEach((item) => item.classList.add('is-visible'));
  }

  // Roving tab focus supports arrow keys, Home, and End for both tab groups.
  function keyboardTabs(tablist) {
    tablist.addEventListener('keydown', (event) => {
      const tabs = [...tablist.querySelectorAll('[role="tab"]')];
      const index = tabs.indexOf(document.activeElement);
      if (index < 0) return;
      let next;
      if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
      else if (event.key === 'ArrowLeft') next = (index + tabs.length - 1) % tabs.length;
      else if (event.key === 'Home') next = 0;
      else if (event.key === 'End') next = tabs.length - 1;
      else return;
      event.preventDefault();
      tabs[next].focus();
      tabs[next].click();
    });
  }
  $$('[role="tablist"]').forEach(keyboardTabs);
  function selectTab(tabs, active, panel) {
    tabs.forEach((tab) => {
      const selected = tab === active;
      tab.setAttribute('aria-selected', String(selected));
      tab.tabIndex = selected ? 0 : -1;
    });
    panel.setAttribute('aria-labelledby', active.id);
  }

  const scenarios = {
    fix: {
      prompt: 'Fix the cart merge bug and add a test.',
      lines: [
        ['tool', '◆ read_file', 'src/cart.rs'],
        ['result', '↳ Found two items sharing the same product ID.'],
        ['tool', '◆ edit_file', 'src/cart.rs · tests/cart.rs'],
        ['answer', '● Merged duplicate items, preserving quantities.'],
        ['success', '✓ All 6 tests pass.'],
      ], cost: '$0.0011', receipt: 'done  ·  $0.0011  ·  ↑5.1k ↓210  ·  5.2s',
    },
    explore: {
      prompt: 'How does a request reach the right handler?',
      lines: [
        ['tool', '◆ repomap', 'src/ · 5,000 B'],
        ['result', '↳ Found the router and its entry points.'],
        ['tool', '◆ read_file', 'src/routes.rs'],
        ['answer', '● The router validates the request, then calls'],
        ['answer', '  the handler matching its method and path.'],
      ], cost: '$0.0002', receipt: 'done  ·  $0.0002  ·  ↑3.8k ↓191  ·  3.4s',
    },
    test: {
      prompt: 'Add tests for the CSV parser’s edge cases.',
      lines: [
        ['tool', '◆ read_file', 'src/parser.rs · tests/parser.rs'],
        ['result', '↳ Covering quoted fields and empty rows.'],
        ['tool', '◆ write_file', 'tests/parser_edges.rs'],
        ['tool', '◆ bash', 'cargo test parser'],
        ['success', '✓ All 8 parser tests pass.'],
      ], cost: '$0.0014', receipt: 'done  ·  $0.0014  ·  ↑6.2k ↓380  ·  6.1s',
    },
  };
  const terminal = $('.terminal-window');
  const prompt = $('#demo-prompt');
  const output = $('#demo-lines');
  const receipt = $('#demo-receipt');
  const state = $('#demo-state');
  const status = $('#demo-status');
  const cost = $('#demo-cost');
  const rail = $('.terminal-rail');
  const toggle = $('#demo-toggle');
  const tabs = $$('[data-scenario]');
  let scenario = scenarios.fix;
  let stage = 'prompt';
  let characters = 0;
  let lineIndex = 0;
  let timer;
  let visible = !('IntersectionObserver' in window);
  let paused = false;
  const canPlay = () => motion && visible && !paused && !document.hidden;
  function appendLine(data, animate = false) {
    const row = document.createElement('p');
    row.className = `demo-${data[0]}${animate ? ' demo-line-enter' : ''}`;
    row.append(document.createTextNode(data[1]));
    if (data[2]) {
      const detail = document.createElement('span');
      detail.textContent = ` ${data[2]}`;
      row.append(detail);
    }
    output.append(row);
  }
  function demoState() {
    const working = stage !== 'rest' && canPlay();
    const isPaused = motion && paused;
    terminal.classList.toggle('is-paused', !canPlay());
    rail.classList.toggle('is-active', working);
    state.classList.toggle('working', working || isPaused);
    state.textContent = isPaused ? 'Ⅱ PAUSED' : working ? '◌ WORKING' : '● READY';
    status.textContent = isPaused ? 'Ⅱ paused' : working ? '◌ working' : '● idle';
    toggle.disabled = !motion;
    toggle.setAttribute('aria-pressed', String(paused));
    toggle.setAttribute('aria-label', !motion ? 'Demo animation disabled' : paused ? 'Resume demo' : 'Pause demo');
    toggle.innerHTML = !motion ? '<span aria-hidden="true">─</span><span class="toggle-label">Static</span>' : paused ? '<span aria-hidden="true">▷</span><span class="toggle-label">Resume</span>' : '<span aria-hidden="true">Ⅱ</span><span class="toggle-label">Pause</span>';
  }
  function showComplete() {
    clearTimeout(timer);
    prompt.textContent = scenario.prompt;
    output.replaceChildren();
    scenario.lines.forEach((data) => appendLine(data));
    receipt.textContent = scenario.receipt;
    cost.textContent = `task ${scenario.cost}`;
    stage = 'rest';
    demoState();
  }
  function resetDemo() {
    clearTimeout(timer);
    stage = 'prompt';
    characters = 0;
    lineIndex = 0;
    prompt.textContent = '';
    output.replaceChildren();
    receipt.textContent = '';
    cost.textContent = 'task $0.0000';
    if (!motion) showComplete();
    else { demoState(); schedule(450); }
  }
  function schedule(delay) {
    clearTimeout(timer);
    if (canPlay()) timer = setTimeout(advance, delay);
  }
  function advance() {
    if (!canPlay()) return;
    if (stage === 'prompt') {
      characters = Math.min(scenario.prompt.length, characters + 2);
      prompt.textContent = scenario.prompt.slice(0, characters);
      if (characters === scenario.prompt.length) { stage = 'lines'; schedule(520); }
      else schedule(40);
    } else if (stage === 'lines') {
      appendLine(scenario.lines[lineIndex++], true);
      if (lineIndex === scenario.lines.length) { stage = 'receipt'; schedule(600); }
      else schedule(650);
    } else if (stage === 'receipt') {
      receipt.textContent = scenario.receipt;
      cost.textContent = `task ${scenario.cost}`;
      stage = 'rest';
      demoState();
      schedule(6500);
    } else resetDemo();
  }
  function playback() {
    clearTimeout(timer);
    demoState();
    if (canPlay()) schedule(stage === 'rest' ? 6500 : 150);
  }
  tabs.forEach((tab) => tab.addEventListener('click', () => {
    selectTab(tabs, tab, $('#demo-panel'));
    scenario = scenarios[tab.dataset.scenario];
    resetDemo();
  }));
  toggle.addEventListener('click', () => { paused = !paused; playback(); });
  document.addEventListener('visibilitychange', playback);
  if ('IntersectionObserver' in window) {
    const demoObserver = new IntersectionObserver(([entry]) => { visible = entry.isIntersecting; playback(); }, { threshold: 0.12 });
    demoObserver.observe(terminal);
  }

  function syncMotion() {
    motion = userMotion && !systemMotion.matches;
    root.classList.toggle('motion-enabled', motion);
    root.classList.toggle('motion-off', !motion);
    motionButton.textContent = systemMotion.matches ? 'Motion: reduced' : motion ? 'Motion: on' : 'Motion: off';
    motionButton.disabled = systemMotion.matches;
    motionButton.setAttribute('aria-pressed', String(!motion));
    if (!motion) {
      revealItems.forEach((item) => item.classList.add('is-visible'));
      showComplete();
    } else resetDemo();
  }
  motionButton.addEventListener('click', () => {
    userMotion = !userMotion;
    try { localStorage.setItem('deeptui-motion', userMotion ? 'on' : 'off'); } catch { /* No persistence needed. */ }
    syncMotion();
  });
  systemMotion.addEventListener('change', syncMotion);
  syncMotion();

  // A tiny light response follows the pointer, without changing layout.
  if (window.matchMedia('(pointer: fine)').matches) {
    let pointerFrame;
    terminal.addEventListener('pointermove', (event) => {
      if (!motion || pointerFrame) return;
      const { clientX, clientY } = event;
      pointerFrame = requestAnimationFrame(() => {
        const bounds = terminal.getBoundingClientRect();
        terminal.style.setProperty('--pointer-x', `${clientX - bounds.left}px`);
        terminal.style.setProperty('--pointer-y', `${clientY - bounds.top}px`);
        pointerFrame = null;
      });
    }, { passive: true });
  }

  const installMethods = {
    macos: {
      caption: 'ONE COMMAND. SHA256-VERIFIED.',
      command: 'curl -fsSL https://raw.githubusercontent.com/lemonhub-io/deeptui/v0.1.0-beta/install.sh | bash',
      help: 'The installer verifies the required beta and migrates legacy ragent settings and usage history. Existing DeepTUI settings are preserved.',
    },
    source: {
      caption: 'BUILD SOMETHING THAT’S YOURS.',
      command: 'git clone https://github.com/lemonhub-io/deeptui.git\ncd deeptui\ncargo build --release\n./target/release/deeptui --init\n./target/release/deeptui',
      help: 'Requires Rust. Linux builds use mold and x86-64-v3 by default; see the README for toolchain options.',
    },
    windows: {
      caption: 'A NATIVE WINDOWS BINARY.',
      command: '# Download and extract the Windows .zip from Releases, then:\n.\\deeptui.exe --init\n.\\deeptui.exe',
      help: 'Choose the x86_64-pc-windows-msvc.zip asset for v0.1.0-beta when available. ',
    },
  };
  const installTabs = $$('[data-install]');
  installTabs.forEach((tab) => tab.addEventListener('click', () => {
    selectTab(installTabs, tab, $('#install-panel'));
    const method = installMethods[tab.dataset.install];
    $('#install-caption').textContent = method.caption;
    $('#install-command').textContent = method.command;
    $('#install-help').textContent = method.help;
    if (tab.dataset.install === 'windows') {
      const link = document.createElement('a');
      link.href = 'https://github.com/lemonhub-io/deeptui/releases';
      link.textContent = 'Open releases ↗';
      $('#install-help').append(link);
    }
  }));
  $$('[data-copy]').forEach((button) => {
    let reset;
    button.addEventListener('click', async () => {
      const source = $(button.dataset.copy);
      const text = source.textContent;
      let copied = false;
      try { await navigator.clipboard.writeText(text); copied = true; } catch {
        const field = document.createElement('textarea');
        field.value = text;
        field.setAttribute('aria-hidden', 'true');
        field.style.cssText = 'position:fixed;left:-9999px;top:0';
        document.body.append(field);
        field.select();
        try { copied = document.execCommand('copy'); } catch { /* Select code below. */ }
        field.remove();
        button.focus();
      }
      clearTimeout(reset);
      button.classList.toggle('done', copied);
      button.querySelector('span').textContent = copied ? 'Copied' : 'Select & copy';
      $('#copy-status').textContent = copied ? 'Installation command copied to clipboard.' : 'Select the installation command and copy it with your keyboard.';
      if (!copied) {
        const range = document.createRange();
        range.selectNodeContents(source);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
      }
      reset = setTimeout(() => { button.querySelector('span').textContent = 'Copy'; button.classList.remove('done'); }, 2200);
    });
  });
})();
