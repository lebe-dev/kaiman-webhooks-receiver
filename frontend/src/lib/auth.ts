const TOKEN_COOKIE = "kwp_token";

export function getToken(): string | null {
  const match = new RegExp(`(?:^|; )${TOKEN_COOKIE}=([^;]*)`).exec(
    document.cookie
  );
  return match ? decodeURIComponent(match[1]) : null;
}

export function setToken(token: string): void {
  document.cookie = `${TOKEN_COOKIE}=${encodeURIComponent(token)}; path=/; SameSite=Strict`;
}

export function clearToken(): void {
  document.cookie = `${TOKEN_COOKIE}=; path=/; max-age=0`;
}

export function isAuthenticated(): boolean {
  return getToken() !== null;
}
