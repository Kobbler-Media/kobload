const tokenKey = 'kobload-token';

let authToken = $state<string | null>(null);
let isAuthed = $state(false);
let loginError = $state('');
let loginLoading = $state(false);

export function getAuthState() {
  return {
    get isAuthed() {
      return isAuthed;
    },
    get loginError() {
      return loginError;
    },
    get loginLoading() {
      return loginLoading;
    },
    get token() {
      return authToken;
    }
  };
}

export function loadAuthToken() {
  if (typeof localStorage === 'undefined') return;
  const stored = localStorage.getItem(tokenKey);
  if (stored && stored.trim()) {
    authToken = stored.trim();
    isAuthed = true;
  }
}

export function setAuthToken(token: string) {
  authToken = token;
  isAuthed = token.trim().length > 0;
  if (typeof localStorage !== 'undefined') {
    if (isAuthed) {
      localStorage.setItem(tokenKey, token);
    } else {
      localStorage.removeItem(tokenKey);
    }
  }
}

export function getAuthToken() {
  return authToken;
}

export function setAuthed(value: boolean) {
  isAuthed = value;
}

export function setLoginError(error: string) {
  loginError = error;
}

export function setLoginLoading(loading: boolean) {
  loginLoading = loading;
}

export function clearAuth() {
  authToken = null;
  isAuthed = false;
  loginError = '';
  loginLoading = false;
  if (typeof localStorage !== 'undefined') {
    localStorage.removeItem(tokenKey);
  }
}
