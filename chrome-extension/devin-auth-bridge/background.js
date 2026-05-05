const WAM_BRIDGE_PORTS = [19876, 19877, 19878, 19879, 19880];
const WAM_EXTENSION_HEADER = 'devin-auth1';
const WAM_RETRY_INTERVAL_MS = 10000;

chrome.runtime.onInstalled.addListener(() => {
  setBadge('idle');
});

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (!message || message.type !== 'DEVIN_AUTH1_SESSION_DETECTED') {
    return false;
  }

  handleDetectedSession(message, sender)
    .then((result) => sendResponse(result))
    .catch((error) => sendResponse({ success: false, status: 'error', message: String(error) }));
  return true;
});

async function handleDetectedSession(message, sender) {
  const token = typeof message.token === 'string' ? message.token.trim() : '';
  if (!token.startsWith('auth1_')) {
    return { success: false, status: 'ignored', message: 'Invalid token' };
  }

  const state = await chrome.storage.local.get(['lastToken', 'lastAttemptAt', 'lastCompletedToken']);
  const now = Date.now();
  if (state.lastCompletedToken === token) {
    return { success: true, status: 'throttled', message: 'Token was already handled' };
  }
  if (state.lastToken === token && state.lastAttemptAt && now - state.lastAttemptAt < WAM_RETRY_INTERVAL_MS) {
    return { success: true, status: 'throttled', message: 'Token was sent recently' };
  }

  await chrome.storage.local.set({
    lastToken: token,
    lastAttemptAt: now,
    lastStatus: 'sending',
    lastMessage: 'Sending Devin token to Windsurf Account Manager...',
    lastEmail: '',
    lastUpdatedAt: new Date().toISOString(),
  });
  setBadge('sending');

  try {
    const result = await postTokenToBridge({
      auth1Token: token,
      userId: message.userId,
      sourceUrl: message.sourceUrl || sender?.tab?.url || '',
    });

    const completedStatuses = new Set(['imported', 'already_exists']);
    const nextStatus = result.status || (result.success ? 'imported' : 'error');
    await chrome.storage.local.set({
      lastStatus: result.status || (result.success ? 'imported' : 'error'),
      lastMessage: result.message || '',
      lastEmail: result.email || '',
      lastPort: result.port || '',
      lastCompletedToken: completedStatuses.has(nextStatus) ? token : state.lastCompletedToken || '',
      lastUpdatedAt: new Date().toISOString(),
    });
    setBadge(nextStatus);
    return result;
  } catch (error) {
    const message = String(error);
    await chrome.storage.local.set({
      lastStatus: 'error',
      lastMessage: message,
      lastUpdatedAt: new Date().toISOString(),
    });
    setBadge('error');
    return { success: false, status: 'error', message };
  }
}

async function postTokenToBridge(payload) {
  let lastError = null;

  for (const port of WAM_BRIDGE_PORTS) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/devin-auth1-token`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'X-WAM-Extension': WAM_EXTENSION_HEADER,
        },
        body: JSON.stringify(payload),
      });
      const data = await response.json();
      if (!response.ok) {
        lastError = new Error(data.message || `Bridge returned ${response.status}`);
        continue;
      }
      return { ...data, port };
    } catch (error) {
      lastError = error;
    }
  }

  throw lastError || new Error('Windsurf Account Manager bridge is not running');
}

function setBadge(status) {
  const badgeMap = {
    idle: { text: '', color: '#6b7280' },
    sending: { text: '...', color: '#f59e0b' },
    imported: { text: 'OK', color: '#22c55e' },
    already_exists: { text: 'EX', color: '#3b82f6' },
    error: { text: 'ERR', color: '#ef4444' },
  };
  const badge = badgeMap[status] || badgeMap.idle;
  chrome.action.setBadgeText({ text: badge.text });
  chrome.action.setBadgeBackgroundColor({ color: badge.color });
}
