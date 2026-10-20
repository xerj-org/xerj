// GET /auth/github/login — start sign-in; see functions/_lib/oauth.js.
import { login } from '../../_lib/oauth.js';
export const onRequestGet = (ctx) => login(ctx, 'github');
