function docsApp() {
  const sections = [
    { id: 'dashboard', category: 'View 1', label: 'Dashboard', title: 'Dashboard', description: 'See the newest LTS, host architecture, default Node, installed versions, and actions that need attention.', tags: ['latest LTS', 'architecture'], commands: [{ key: 'r', action: 'Refresh machine state' }, { key: 'l', action: 'Install latest native LTS' }] },
    { id: 'packages', category: 'View 2', label: 'Packages', title: 'Global packages', description: 'Scan every installed nvm Node version and compare global packages found on each one.', tags: ['npm', 'scan'], commands: [{ key: '↑ / ↓', action: 'Navigate packages' }, { key: 'g', action: 'Sync package registry' }] },
    { id: 'registry', category: 'View 3', label: 'Registry', title: 'Package registry', description: 'Review the consolidated registry before restoring packages into the current default Node.', tags: ['JSON', 'restore'], commands: [{ key: 'a', action: 'Restore registry packages' }, { key: 'Enter', action: 'Activate selected action' }] },
    { id: 'updates', category: 'View 4', label: 'Updates', title: 'Package updates', description: 'Check global npm packages for newer versions and approve updates one at a time.', tags: ['safe updates', 'confirmation'], commands: [{ key: 'u', action: 'Check package updates' }, { key: 'Enter', action: 'Confirm selected update' }] },
    { id: 'activity', category: 'View 5', label: 'Activity', title: 'Activity log', description: 'Follow refreshes, installs, scans, restores, and update results without losing context.', tags: ['status', 'feedback'], commands: [{ key: 'j / k', action: 'Scroll activity' }, { key: 'r', action: 'Refresh activity' }] },
    { id: 'shortcuts', category: 'Keyboard', label: 'Keyboard shortcuts', title: 'Keyboard shortcuts', description: 'The TUI supports keyboard navigation, mouse selection, and wheel scrolling.', tags: ['keyboard', 'mouse'], commands: [{ key: '1–5', action: 'Switch views' }, { key: 'q / Esc', action: 'Quit or close a dialog' }, { key: 'j / k', action: 'Navigate and scroll' }] }
  ];
  return {
    q: '', menuOpen: false, sections,
    views: sections.slice(0, 5).map(s => ({ key: s.category.replace('View ', ''), name: s.label, description: s.description })),
    get filteredCount() { return this.q ? sections.filter(s => this.matches(s)).length : sections.length; },
    matches(section) { const q = this.q.toLowerCase().trim(); if (!q) return true; return [section.title, section.label, section.category, section.description, ...section.tags, ...section.commands.map(c => `${c.key} ${c.action}`)].join(' ').toLowerCase().includes(q); },
    matchesId(id) { const q = this.q.toLowerCase().trim(); return !q || id.includes(q) || ({ quickstart: 'install cargo run', installation: 'install cargo rust', usage: 'usage views keyboard', architecture: 'architecture registry restore', configuration: 'configuration nvm registry', troubleshooting: 'troubleshooting error nvm', reference: 'reference github crates' }[id] || '').includes(q); },
    init() { const input = document.querySelector('.compact-search input'); document.addEventListener('keydown', e => { if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); input?.focus(); } if (e.key === '/' && !/INPUT|TEXTAREA/.test(document.activeElement.tagName)) { e.preventDefault(); input?.focus(); } }); },
    copyBlock(button) { const code = button.nextElementSibling?.innerText || ''; navigator.clipboard?.writeText(code); button.textContent = 'Copied'; setTimeout(() => button.textContent = 'Copy', 1200); }
  };
}
