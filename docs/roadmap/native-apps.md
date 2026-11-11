# Native apps (designed, not built)

Swifty Home 0.1.0 has one way in: a passkey in a browser, with a session cookie.
This page is the design for phone and desktop apps, so the API does not need to
change when they come. Back to the [roadmap](../roadmap.md).

## What is already in place

- One versioned, documented API under `/api/v1`, with no assumptions that only the
  web page could meet.
- `GET /api/v1/meta`, so an app can see what a hub supports before it signs in:
  product, version, API version, whether it is live, how it authenticates, and that
  it offers an event stream.
- A single authentication extractor in the hub. Every handler receives a principal
  (person, session, and how they came in) and never looks at the cookie itself, so a
  second way in is added in one place.

## The plan

1. **Pairing.** The owner runs `app.pair`, a critical action, in the web app. It
   shows a QR code.
2. **An app credential.** The app creates a P-256 key held in the phone's secure
   hardware and registers its public half with the hub. App credentials are listed
   and revoked beside passkeys.
3. **Short-lived tokens.** The app signs a challenge with that key to get a bearer
   token that lives ten minutes.
4. **Rules for bearer requests.** They skip the `Origin` check, which is a browser
   defence; a request carrying both a cookie and a bearer token is refused.
5. **Tiers stay the same.** Tier 2 confirms in the app. Tier 3 opens the system
   browser for a real passkey assertion bound to the action, exactly as the web app
   does.

Until then, a desktop shell can wrap the web app as it is.
