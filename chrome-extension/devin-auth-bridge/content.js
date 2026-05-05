const WAM_DEVIN_SESSION_KEY = 'auth1_session';
const WAM_RESEND_INTERVAL_MS = 10000;
let wamLastSeenToken = '';
let wamLastSentAt = 0;
let wamScanTimer = null;

function wamParseSession(rawValue) {
  if (!rawValue) return null;
  try {
    const parsed = JSON.parse(rawValue);
    const token = typeof parsed.token === 'string' ? parsed.token.trim() : '';
    const userId = typeof parsed.userId === 'string' ? parsed.userId : undefined;
    if (!token.startsWith('auth1_')) return null;
    return { token, userId };
  } catch {
    return null;
  }
}

function wamSendSession(session) {
  chrome.runtime.sendMessage(
    {
      type: 'DEVIN_AUTH1_SESSION_DETECTED',
      token: session.token,
      userId: session.userId,
      sourceUrl: window.location.href,
    },
    () => {
      void chrome.runtime.lastError;
    },
  );
}

function wamScanForSession() {
  const session = wamParseSession(window.localStorage.getItem(WAM_DEVIN_SESSION_KEY));
  if (!session) return;
  const now = Date.now();
  if (session.token === wamLastSeenToken && now - wamLastSentAt < WAM_RESEND_INTERVAL_MS) return;
  wamLastSeenToken = session.token;
  wamLastSentAt = now;
  wamSendSession(session);
}

function wamScheduleScan(delay = 250) {
  if (wamScanTimer !== null) {
    window.clearTimeout(wamScanTimer);
  }
  wamScanTimer = window.setTimeout(() => {
    wamScanTimer = null;
    wamScanForSession();
  }, delay);
}

window.addEventListener('storage', (event) => {
  if (event.key === WAM_DEVIN_SESSION_KEY) {
    wamScheduleScan(50);
  }
});

document.addEventListener('visibilitychange', () => {
  if (!document.hidden) {
    wamScheduleScan(50);
  }
});

wamScheduleScan(50);
window.setInterval(wamScanForSession, 1000);
