const statusEl = document.getElementById('status');
const emailEl = document.getElementById('email');
const portEl = document.getElementById('port');
const updatedEl = document.getElementById('updated');
const messageEl = document.getElementById('message');
const openDevinButton = document.getElementById('openDevin');

const statusLabels = {
  sending: 'Sending',
  imported: 'Imported',
  already_exists: 'Already Added',
  throttled: 'Waiting',
  error: 'Error',
};

async function renderStatus() {
  const state = await chrome.storage.local.get([
    'lastStatus',
    'lastMessage',
    'lastEmail',
    'lastPort',
    'lastUpdatedAt',
  ]);
  const status = state.lastStatus || 'idle';
  statusEl.textContent = statusLabels[status] || 'Idle';
  emailEl.textContent = state.lastEmail || '-';
  portEl.textContent = state.lastPort ? String(state.lastPort) : '-';
  updatedEl.textContent = state.lastUpdatedAt ? new Date(state.lastUpdatedAt).toLocaleString() : '-';
  messageEl.textContent = state.lastMessage || 'Open Devin and sign in. Keep Windsurf Account Manager running.';
}

openDevinButton.addEventListener('click', () => {
  chrome.tabs.create({ url: 'https://app.devin.ai/' });
});

renderStatus();
chrome.storage.onChanged.addListener(renderStatus);
