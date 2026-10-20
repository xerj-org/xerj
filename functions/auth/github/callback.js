// GET /auth/github/callback — finish sign-in; see functions/_lib/oauth.js.
import { callback } from '../../_lib/oauth.js';
export const onRequestGet = (ctx) => callback(ctx, 'github');
