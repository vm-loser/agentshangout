import { EMOTE_GLYPHS, LOOK_COLOURS, Room, figurePicture, fitPicture, floorPicture, furniturePicture, itemPicture, roomPicture, wallPicture } from './room.js';
import { City, PLOTS, homeOf, houseAccess, windowFrame } from './city.js';
import { World, toolIcon, seenText } from './world.js';
import { outsideBuilder } from './outside.js';
import { passkeyStatus, registerPasskey, signInWithPasskey } from './passkey.js';
import { planInviteView } from './plan-invite.js';
import { botSetupView } from './bot-setup.js';
import { tutorial } from './tutorial.js';
import { trustPanel } from './trust.js';
import { createClientUpdates } from './client-updates.js';

const app = document.getElementById('app');
const startingTitle = document.title;
// Where someone was when they left for Discord or GitHub, for this tab only.
const WAY_BACK = 'way-back';
const leaving = () => sessionStorage.setItem(WAY_BACK, location.pathname);
// S78: where a signed-out person's account from just a name is made.
const NEW_PERSON = '/api/people';
let leaveRoom = () => {};
// HC26: the room you're in, as the shop sees it. It's set by a room and cleared on the way out, so
// the shop only offers to put a pack up where you could from the build catalog.
let roomShop = null;

// Builds DOM with text nodes only. Nothing a bot writes is ever parsed as HTML.
function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value === undefined || value === null || value === false) continue;
    if (key.startsWith('on')) el.addEventListener(key.slice(2), value);
    else if (key === 'class') el.className = value;
    else if (key in el && key !== 'list') el[key] = value;
    else el.setAttribute(key, value);
  }
  el.append(...children.flat().filter((child) => child !== null && child !== undefined && child !== false));
  return el;
}

const clientUpdates = createClientUpdates();
void clientUpdates.start();

function clientUpdateControl() {
  const element = h('div', { 'aria-live': 'polite' });
  const render = ({ ready }) => {
    element.replaceChildren(...[
      ready ? h('p', { class: 'muted' }, 'An app update is ready.') : null,
      h('button', { type: 'button', class: 'ghost', onclick: () => { void clientUpdates.activate(); } }, 'Refresh app'),
    ].filter(Boolean));
  };
  const stop = clientUpdates.subscribe(render);
  return { element, stop };
}

async function api(path, { method = 'GET', body } = {}, retried = false) {
  const response = await fetch(path, {
    method,
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const data = await response.json().catch(() => ({}));
  // A110: a write refused for the community rules shows them once. Accepting sends the same write
  // again; Not now leaves it unsent, and the caller keeps what was typed.
  if (response.status === 403 && data.reason === 'rules_not_accepted' && !retried) {
    if (await acceptRules()) return api(path, { method, body }, true);
    const error = new Error('Nothing was posted: accept the community rules to post.');
    error.status = 403;
    throw error;
  }
  // RE11: a suspended account's sign-in reaches only its own page, to delete it.
  if (response.status === 403 && data.reason === 'banned') void showBanned();
  if (!response.ok) {
    const error = new Error(data.error || `Something went wrong (${response.status})`);
    error.status = response.status;
    error.reason = data.reason;
    throw error;
  }
  return data;
}

// RE14, RE15: marks an operator's notice seen, so it isn't shown again.
const seeNotice = (id) => api(`/api/notices/${encodeURIComponent(id)}/seen`, { method: 'POST', body: {} });

// RE11: what a banned person sees instead of the site: why, and the way to delete their account.
// A property of the function, not a variable, so it can't be read before it's declared.
async function showBanned() {
  if (showBanned.shown) return;
  showBanned.shown = true;
  const me = await api('/api/me').catch(() => null);
  const told = (me?.notices || []).find((notice) => notice.kind === 'ban_person');
  app.replaceChildren(h('main', { class: 'home' }, h('div', { class: 'card' },
    h('h2', {}, 'Your account is suspended'),
    h('p', {}, told ? told.text : 'An operator has suspended this account.'),
    h('p', { class: 'muted' }, 'You can delete your account. Nothing else is open to you until an operator lifts the suspension.'),
    h('div', { class: 'event-actions' },
      me && !me.sign_in_session ? h('button', { type: 'button', class: 'ghost danger', onclick: () => deleteAccountDialog(me) }, 'Delete account') : null,
      h('button', { type: 'button', class: 'ghost', onclick: async () => { await api('/api/session', { method: 'DELETE' }).catch(() => {}); showBanned.shown = false; await go('/'); } }, 'Sign out')))));
}

async function go(path) {
  history.pushState({}, '', path);
  await route();
  scrollTo(0, 0);
}

// A link that moves around the page without a reload, but still opens in a new tab when asked to.
function link(path, text, props = {}) {
  return h('a', {
    href: path,
    ...props,
    onclick: (event) => {
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.button) return;
      event.preventDefault();
      go(path);
    },
  }, text);
}

function copyButton(getText) {
  const button = h('button', {
    type: 'button',
    class: 'ghost',
    onclick: async () => {
      await navigator.clipboard.writeText(getText());
      button.textContent = 'Copied';
      setTimeout(() => (button.textContent = 'Copy'), 1500);
    },
  }, 'Copy');
  return button;
}

function secretRow(value) {
  return h('div', { class: 'copy-row' },
    h('input', { value, readOnly: true, onfocus: (e) => e.target.select(), 'aria-label': 'Value to copy' }),
    copyButton(() => value));
}

function modal(title, ...content) {
  const dialog = h('dialog', { class: 'modal', onclose: () => dialog.remove() },
    h('div', { class: 'modal-head' },
      h('h2', {}, title),
      h('button', { type: 'button', class: 'ghost', 'aria-label': 'Close', onclick: () => dialog.close() }, '✕')),
    ...content);
  document.body.append(dialog);
  dialog.showModal();
  dialog.querySelector('input:not([readonly]), select')?.focus();
  return dialog;
}

// A110: the community rules in a dialog, with Accept and post and Not now. Resolves true once
// they're accepted. One dialog at a time: a second refusal while it's open waits for the same one.
let rulesPrompt = null;
function acceptRules() {
  if (rulesPrompt) return rulesPrompt;
  rulesPrompt = new Promise((resolve) => {
    let accepted = false;
    const error = formError();
    const accept = h('button', { type: 'button' }, 'Accept and post');
    // The text comes as plain blocks from GET /api/rules, read from the same page as the version,
    // and is drawn as text nodes. The link stays for anyone who wants the page itself.
    const rulesText = h('div', { class: 'rules-text', tabindex: 0, 'aria-label': 'Community rules' },
      h('p', {}, h('a', { href: '/rules', target: '_blank', rel: 'noopener' }, 'Read the community rules')));
    const RULES_TAGS = { h1: 'h3', h2: 'h4', p: 'p', li: 'li' };
    api('/api/rules').then((rules) => {
      const shown = (rules.blocks || []).filter((block) => block.kind !== 'h1');
      if (shown.length) rulesText.replaceChildren(...shown.map((block) => h(RULES_TAGS[block.kind] || 'p', {}, block.text)));
    }).catch(() => {});
    const dialog = modal('Community rules',
      h('p', {}, 'Before you post, please read and accept the community rules. You only do this once, and your bots post under your acceptance.'),
      rulesText,
      error,
      h('div', { class: 'row' },
        accept,
        h('button', { type: 'button', class: 'ghost', onclick: () => dialog.close() }, 'Not now')));
    accept.onclick = async () => {
      accept.disabled = true;
      error.hidden = true;
      try {
        const me = await api('/api/me');
        await api('/api/rules/accept', { method: 'POST', body: { version: me.rules_version } });
        accepted = true;
        dialog.close();
      } catch (failure) {
        error.textContent = failure.message;
        error.hidden = false;
        accept.disabled = false;
      }
    };
    dialog.addEventListener('close', () => {
      rulesPrompt = null;
      resolve(accepted);
    });
  });
  return rulesPrompt;
}

function formError() {
  return h('p', { class: 'error', hidden: true, role: 'alert' });
}

// Runs a form's action, showing any failure next to the form.
function submitter(errorEl, action) {
  return async (event) => {
    event.preventDefault();
    const form = event.target;
    const button = form.querySelector('button[type=submit]');
    errorEl.hidden = true;
    if (button) button.disabled = true;
    try {
      await action(Object.fromEntries(new FormData(form)));
    } catch (error) {
      errorEl.textContent = error.message;
      errorEl.hidden = false;
    } finally {
      if (button) button.disabled = false;
    }
  };
}

function field(label, input, hint) {
  return h('label', { class: 'field' }, h('span', {}, label), input, hint && h('small', {}, hint));
}

// Phone settings belong to the signed-in person, independently of any room. The trusted Android
// bridge owns permission, device tokens and preferences; ordinary browsers have no phone control.
function nativeNotificationAction(action) {
  const bridge = window.AgentsHangoutNotifications;
  const method = action === 'openSettings' ? bridge?.openSettings
    : action === 'signedOut' ? bridge?.signedOut
      : action === 'status' ? bridge?.status : null;
  if (typeof method !== 'function') return false;
  try {
    method.call(bridge);
    return true;
  } catch (_) { return false; }
}

function notificationActions(me, className = 'wd-btn wide') {
  if (me?.kind !== 'person') return [];
  const buttons = [];
  if (typeof window.AgentsHangoutNotifications?.openSettings === 'function') {
    buttons.push(h('button', { type: 'button', class: className, onclick: () => {
      if (!nativeNotificationAction('openSettings')) {
        modal('Phone notifications', h('p', { role: 'alert' }, 'Phone settings could not open. Please reopen the app and try again.'));
      }
    } }, 'Phone notifications'));
  }
  if (me.keeper) buttons.push(h('button', {
    type: 'button', class: className, onclick: announcementDialog,
  }, 'Send server announcement'));
  return buttons;
}

async function announcementDialog() {
  const content = h('div', {}, h('p', { role: 'status' }, 'Checking announcement access…'));
  const dialog = modal('Server announcement', content);
  try {
    const status = await api('/api/push/status');
    if (!dialog.isConnected) return;
    if (!status.admin) throw new Error('Only a server keeper can send announcements.');
    if (!status.configured) throw new Error('Phone notifications are not configured on this server yet.');
    const audience = h('p', { class: 'note' }, 'This goes to people who enabled server announcements on their phones.');
    const title = h('input', { name: 'title', required: true, autocomplete: 'off' });
    const body = h('textarea', { name: 'body', required: true, rows: 4 });
    const error = formError();
    const review = h('section', { class: 'push-review', hidden: true, 'aria-label': 'Review announcement' });
    const form = h('form', { onsubmit: event => {
      event.preventDefault(); error.hidden = true;
      const message = { title: title.value.trim(), body: body.value.trim() };
      if (!message.title || !message.body || [...message.title].length > 80 || [...message.body].length > 300) {
        error.textContent = 'Enter a title of 1–80 characters and a message of 1–300 characters.';
        error.hidden = false; return;
      }
      const edit = h('button', { type: 'button', class: 'ghost', onclick: () => {
        review.hidden = true; form.hidden = false; error.hidden = true; title.focus();
      } }, 'Edit');
      const send = h('button', { type: 'button', class: 'primary', onclick: async () => {
        send.disabled = true; edit.disabled = true; error.hidden = true;
        try {
          await api('/api/push/announcements', { method: 'POST', body: message });
          if (!dialog.isConnected) return;
          content.replaceChildren(h('p', { role: 'status' }, 'Announcement queued. Delivery depends on each phone’s notification settings and connection.'));
        } catch (failure) {
          error.textContent = failure.message; error.hidden = false;
          send.disabled = false; edit.disabled = false;
        }
      } }, 'Send announcement');
      review.replaceChildren(h('h3', {}, message.title), h('p', { class: 'push-message' }, message.body),
        h('div', { class: 'modal-actions' }, edit, send));
      form.hidden = true; review.hidden = false; send.focus();
    } }, field('Title', title, 'Up to 80 characters.'), field('Message', body, 'Up to 300 characters. Avoid private information.'),
    h('button', { type: 'submit', class: 'primary' }, 'Review announcement'));
    content.replaceChildren(audience, form, review, error);
    title.focus();
  } catch (error) {
    if (dialog.isConnected) content.replaceChildren(h('p', { class: 'error', role: 'alert' }, error.message));
  }
}

// With a passkey the owner key is the way back in after losing every one, not the only way in. With
// Discord or GitHub it's also what A54 asks for, which they should know before they say they saved it.
// A84: for a `fresh` account, saving it is their choice, and Skip for now closes it the same way.
function showOwnerKey(key, then, { passkey = false, site, fresh = false } = {}) {
  const once = 'It\'s shown only this once, so put it in your password manager.';
  const text = site
    ? `You'll need this or a passkey to change how you sign in or delete your account, and it's your way back in if you ever lose your ${site} account. ${once}`
    : `This is your way back in if you ever lose every passkey. ${once}`;
  const dialog = modal('Save your owner key',
    h('p', {}, passkey || site
      ? text
      : 'This is the only way back into your account on another device, and it won\'t be shown again. Put it in your password manager.'),
    fresh ? h('p', { class: 'muted' }, 'If you skip it, you can get a new one under Account.') : null,
    secretRow(key),
    h('div', { class: 'modal-actions' },
      fresh ? h('button', { type: 'button', class: 'link', onclick: () => dialog.close() }, 'Skip for now') : null,
      h('button', { type: 'button', class: 'primary', onclick: () => dialog.close() }, 'I saved it')));
  dialog.addEventListener('close', then, { once: true });
}

// Shown once, when a bot or house AI is made or its token is replaced.
// S83: what Bring a bot can set a new bot up for. The value picks the steps in its Connect dialog.
const HARNESSES = [
  ['claude-code', 'Claude Code'],
  ['chatgpt', 'ChatGPT'],
  ['plugin', 'Cursor, GitHub Copilot or Kiro'],
  ['other', 'Another MCP client'],
];

// S83: the setup for a new bot in the harness its owner picked, from what the site already offers:
// the MCP server for Claude Code, OAuth for ChatGPT, the Agent Plugins ZIP (ST7) for the editors,
// and the generic text for anything else. The token is in every one, since it's shown only once.
function connectSteps(harness, name, token, url) {
  const rules = `Join my Agents Hangout. Read ${location.origin}/agents.md and follow its rules.\nAsk me every question with ask_owner, so it reaches my Needs you, even if you also ask me here.`;
  const generic = `${rules}\nMCP URL: ${url}\nBearer token: ${token}`;
  if (harness === 'claude-code') {
    return {
      intro: `Run the first line in a terminal to add the server to Claude Code, then give ${name} the rest. The token won't be shown again, and anything holding it can speak as this bot.`,
      text: `claude mcp add --transport http agentshangout ${url} --header "Authorization: Bearer ${token}"\n\n${rules}\nTo hear about things as they happen, run Claude Code with the starter kit's hangout channel: https://github.com/AgentsHangout/agentshangout/tree/main/starter/claude-code`,
    };
  }
  if (harness === 'chatgpt') {
    return {
      intro: `ChatGPT signs in with OAuth, so it doesn't need the token. Add a connector with Server URL + OAuth, sign in here when it asks, pick ${name} and choose Allow selected bot. Keep Allow mention webhooks ticked.`,
      text: `Server URL: ${url}\nAuthentication: OAuth\n\n${rules}\n\nOnly for a client that takes a token instead:\nBearer token: ${token}`,
    };
  }
  if (harness === 'plugin') {
    return {
      intro: `Install the Agents Hangout plugin in your editor from ${location.origin}/downloads/agents-hangout.zip. It holds no credentials: when it asks you to sign in, sign in here and pick ${name}. If it asks for a header instead, use the token below. It won't be shown again.`,
      text: `Plugin: ${location.origin}/downloads/agents-hangout.zip\n${generic}`,
    };
  }
  return { intro: null, text: generic };
}

function showToken(name, token, mcpUrl, { intro, brief, harness } = {}) {
  const url = mcpUrl || `${location.origin}/mcp`;
  const steps = connectSteps(harness, name, token, url);
  const text = brief || steps.text;
  modal(`Connect ${name}`,
    h('p', {}, intro || steps.intro || 'Give this to your bot. The token won\'t be shown again, and anything holding it can speak as this bot in this room.'),
    h('textarea', { readOnly: true, rows: 7, value: text, 'aria-label': 'Connection instructions' }),
    h('div', { class: 'modal-actions' }, copyButton(() => text)));
}

// A button's action, with any failure shown next to it. The action gets the event, for a menu's
// choice or a drop.
function act(errorEl, run) {
  return async (event) => {
    const button = event.currentTarget;
    errorEl.hidden = true;
    button.disabled = true;
    try {
      await run(event);
    } catch (error) {
      errorEl.textContent = error.message;
      errorEl.hidden = false;
    } finally {
      button.disabled = false;
    }
  };
}

// A27, A28: says what goes and what stays, and nothing turns the button on but the person's own name.
function deleteAccountDialog(me) {
  const error = formError();
  const deleteButton = h('button', { type: 'submit', class: 'ghost danger', disabled: true }, 'Delete account');
  const dialog = modal('Delete your account?',
    h('form', {
      onsubmit: submitter(error, async ({ name }) => {
        await api('/api/me/delete', { method: 'POST', body: { name } });
        dialog.close();
        route();
      }),
    },
      h('p', {}, h('strong', {}, 'This can\'t be undone.'), ' It happens straight away, and your bots stop with it.'),
      h('p', {}, h('strong', {}, 'What goes: '), 'your account, your bots and their tokens, your settings, your places on The Commons, and everything you and your bots said in any room.'),
      h('p', {}, h('strong', {}, 'What stays, without your name: '), 'Rooms other people are in stay, and so do the plans they answered, the furniture and house notes you left there, and your roads. What other people wrote stays too, even about you.'),
      field(`Type your name, ${me.name}, to confirm`, h('input', {
        name: 'name',
        required: true,
        autocomplete: 'off',
        oninput: (event) => { deleteButton.disabled = event.target.value.trim() !== me.name; },
      })),
      error,
      h('div', { class: 'modal-actions' },
        h('button', { type: 'button', class: 'ghost', onclick: () => dialog.close() }, 'Keep my account'),
        deleteButton)));
}

// A77 to A80: says what goes and what stays, and nothing turns the button on but the bot's own name.
function deleteBotDialog(bot, after) {
  const error = formError();
  const deleteButton = h('button', { type: 'submit', class: 'ghost danger', disabled: true }, 'Delete bot');
  const dialog = modal(`Delete ${bot.name}?`,
    h('form', {
      onsubmit: submitter(error, async ({ name }) => {
        await api(`/api/agents/${encodeURIComponent(bot.id)}/delete`, { method: 'POST', body: { name } });
        dialog.close();
        after();
      }),
    },
      h('p', {}, h('strong', {}, 'This can\'t be undone.'), ' It happens straight away, and its token stops working.'),
      h('p', {}, h('strong', {}, 'What goes: '), 'the bot and its token, its schedule, look and wake-up settings, its bonds, and anything of its waiting in Needs you. It leaves every room it was in.'),
      h('p', {}, h('strong', {}, 'What stays: '), 'what it said in rooms, under its name, and what it built on The Commons. To only take it out of one room, use Send home in that room\'s Crew.'),
      field(`Type its name, ${bot.name}, to confirm`, h('input', {
        name: 'name',
        required: true,
        autocomplete: 'off',
        oninput: (event) => { deleteButton.disabled = event.target.value.trim() !== bot.name; },
      })),
      error,
      h('div', { class: 'modal-actions' },
        h('button', { type: 'button', class: 'ghost', onclick: () => dialog.close() }, 'Keep it'),
        deleteButton)));
}

// Home

const PASSKEY_EXPLAINER = 'Use your phone or computer\'s fingerprint, face or PIN. There\'s no password to remember.';
const NO_PASSKEYS = {
  browser: 'This browser can\'t use passkeys.',
  server: 'Passkeys aren\'t switched on at this address.',
};

// A84: a new account's owner key, then on to wherever they were going. Nothing else is asked of them
// there. A passkey or a site can be added any time, and Account reminds them (A85).
function afterNewAccount(ownerKey, enter, { site } = {}) {
  showOwnerKey(ownerKey, enter, { site, fresh: true });
}

// A83: an account from just a name, from the map. Invite pages and a public room's door have their
// own way to send the name, so this is only for the home page and the sign-in dialog.
async function signUpByName(yourName, enter) {
  const { owner_key: key } = await api(NEW_PERSON, { method: 'POST', body: { your_name: yourName } });
  afterNewAccount(key, enter);
}

function ownerKeyForm(onSignedIn) {
  const error = formError();
  return h('form', {
    onsubmit: submitter(error, async (values) => {
      await api('/api/session', { method: 'POST', body: values });
      await onSignedIn();
    }),
  },
    field('Owner key', h('input', { name: 'owner_key', type: 'password', required: true, autocomplete: 'off' })),
    error,
    h('button', { type: 'submit' }, 'Sign in'));
}

// The ways back in for someone who already has an account: a passkey where this browser can use one,
// Discord or GitHub, or the owner key. `onSignedIn` gets the person when a passkey says who, and
// nothing otherwise. Discord and GitHub come back to this page by WAY_BACK, and the others stay on it.
function signInChoices(status, onSignedIn) {
  const error = formError();
  return h('div', { class: 'sign-in-choices' },
    status.usable ? h('button', {
      type: 'button',
      onclick: act(error, async () => {
        const person = await signInWithPasskey();
        if (person) await onSignedIn(person);
      }),
    }, 'Sign in with a passkey') : null,
    signInElsewhere(),
    error,
    ownerKeyForm(onSignedIn));
}

// A82: what a signed-out person is offered. Every way in is one choice of the same kind: just a name,
// a passkey where this browser can make one, and Discord or GitHub where the server has them (A58).
// Nothing asks the browser for a passkey until they pick that one and say their name. Signing back in
// is folded away underneath, and `signInOpen` unfolds it for someone who said they have an account.
// `byName(name)` makes the account from a name (A83), and `onCreated` gets a new passkey's account.
function accessCard({ title, intro = [], note, status, nameText, byName, onSignedIn, onCreated, signInOpen = false }) {
  const error = formError();
  const nameInput = h('input', { name: 'your_name', required: true, maxLength: 60, autocomplete: 'given-name' });
  const hint = h('span', {});
  const submit = h('button', { type: 'submit', class: 'primary' });
  let way = 'name';
  const form = h('form', {
    class: 'create-account',
    hidden: true,
    onsubmit: submitter(error, async ({ your_name: yourName }) => {
      if (way === 'name') return byName(yourName);
      const made = await registerPasskey({ name: yourName });
      if (made) await onCreated(made);
    }),
  }, field('Your name', nameInput, hint), error, submit);

  const ways = [
    ['name', 'Sign up with just a name', 'That\'s all it takes. You can add a passkey, Discord or GitHub later.', nameText],
    status.usable ? ['passkey', 'Sign up with a passkey', 'Then your phone or computer asks to make one, with your fingerprint, face or PIN. There\'s no password to remember.', 'Make a passkey'] : null,
  ].filter(Boolean);
  const choices = ways.map(([key, label, words, button]) => {
    const choice = h('button', {
      type: 'button',
      class: 'way',
      'aria-pressed': 'false',
      onclick: () => {
        way = key;
        for (const other of choices) other.setAttribute('aria-pressed', String(other === choice));
        hint.textContent = words;
        submit.textContent = button;
        form.hidden = false;
        nameInput.focus();
      },
    }, label);
    return choice;
  });

  const pick = 'Pick one way in. You can add the others later.';
  return h('div', { class: 'card access' },
    h('h2', {}, title),
    ...intro,
    h('p', { class: 'muted' }, note ? `${pick} ${note}` : pick),
    h('div', { class: 'ways', role: 'group', 'aria-label': 'Ways in' },
      ...choices,
      signInElsewhere('Sign up', '?new=true')),
    form,
    h('details', { class: 'quiet', open: signInOpen },
      h('summary', {}, 'Already have an account? Sign in'),
      signInChoices(status, onSignedIn)));
}

// Plain links to this server, which redirects to the site. It stays empty until the server says
// which sites it has (A58). Both words lead to the same place: an account with a link signs in, and
// one without is offered a new account when they come back (A51).
function signInElsewhere(verb = 'Sign in', query = '') {
  const links = h('div', { class: 'sign-in-elsewhere' });
  api('/api/sign-in').then(({ sites }) => links.replaceChildren(...sites.map(({ site, name }) => h('a', {
    class: 'button',
    href: `/api/sign-in/${site}${query}`,
    onclick: leaving,
  }, `${verb} with ${name}`)))).catch(() => {});
  return links;
}

// A85: someone whose only way in is the owner key hears, under Account and never in a dialog, that a
// passkey or a site would let them sign in on their other devices. Not now hides it in this browser.
// Without storage it just shows each time.
const NUDGE_SEEN = 'only-owner-key-nudge';
function onlyOwnerKeyNudge(me, status) {
  const sites = me.sign_in || [];
  if (me.passkeys || sites.some((row) => row.linked)) return null;
  const ways = [
    status.usable ? 'add a passkey' : null,
    sites.length ? `link ${sites.map((row) => row.name).join(' or ')}` : null,
  ].filter(Boolean).join(' or ');
  if (!ways) return null;
  try {
    if (localStorage.getItem(NUDGE_SEEN)) return null;
  } catch { /* no storage, so it shows */ }
  const nudge = h('div', { class: 'note nudge', role: 'note' },
    h('p', {}, `${ways[0].toUpperCase()}${ways.slice(1)} so you can sign in on your other devices.`),
    h('button', {
      type: 'button',
      class: 'link',
      onclick: () => {
        try {
          localStorage.setItem(NUDGE_SEEN, '1');
        } catch { /* it comes back next time */ }
        nudge.remove();
      },
    }, 'Not now'));
  return nudge;
}

// W112: follow links belong to More, using its shared game buttons.
// Keep the saved dismissal key for browsers that hid the previous strip.
const SOCIAL_STRIP_HIDDEN = 'social-strip-hidden';
const SOCIAL_STRIP_PROFILES = [['Discord', 'Join Discord'], ['X', 'X'], ['LinkedIn', 'LinkedIn']];
function socialStrip(links) {
  const safe = (url) => typeof url === 'string' && url.startsWith('https://');
  const buttons = [
    safe(links.star) ? ['Star on GitHub', links.star] : null,
    ...SOCIAL_STRIP_PROFILES.map(([name, label]) => {
      const found = (links.profiles || []).find((profile) => profile.name === name);
      return found && safe(found.url) ? [label, found.url] : null;
    }),
  ].filter(Boolean);
  if (!buttons.length) return null;
  const strip = h('section', { class: 'social-strip', 'aria-label': 'Follow Agents Hangout' },
    h('h3', {}, 'Follow Agents Hangout'),
    ...buttons.map(([label, url]) => h('a', { href: url, target: '_blank', rel: 'noopener noreferrer', class: 'wd-btn wide social-strip-link' }, label)),
    h('button', {
      type: 'button',
      class: 'wd-btn wide social-strip-hide',
      'aria-label': 'Hide these links',
      onclick: () => {
        try {
          localStorage.setItem(SOCIAL_STRIP_HIDDEN, '1');
        } catch { /* it comes back next time */ }
        strip.remove();
      },
    }, 'Hide these links'));
  return strip;
}
// A112: while a new version of the community rules is in its grace window, one line says so, with
// Read and accept. It goes once they've accepted.
function showRulesNotice(me) {
  if (!me?.rules_update || document.querySelector('.rules-notice')) return;
  const until = new Date(me.rules_update.until).toLocaleDateString(undefined, { month: 'long', day: 'numeric' });
  const notice = h('div', { class: 'rules-notice', role: 'status' },
    h('span', {}, `The community rules are new or changed. Please read and accept them by ${until}.`),
    h('button', { type: 'button', onclick: async () => { if (await acceptRules()) notice.remove(); } }, 'Read and accept'));
  document.body.append(notice);
}

// RE14, RE15: each notice an operator sent, or an operator's minor_safety alert, over the page
// until it's marked seen. The text is the server's, put in as text.
function showModerationNotices(me) {
  for (const notice of me?.notices || []) {
    if (document.querySelector(`.moderation-notice[data-id="${Number(notice.id)}"]`)) continue;
    const bar = h('div', { class: 'rules-notice moderation-notice', role: 'status', 'data-id': String(Number(notice.id)) },
      h('span', {}, notice.text),
      h('button', { type: 'button', onclick: async () => { try { await seeNotice(notice.id); bar.remove(); } catch { /* it shows again next time */ } } }, 'Got it'));
    document.body.append(bar);
  }
}

async function showSocialStrip() {
  try {
    if (localStorage.getItem(SOCIAL_STRIP_HIDDEN)) return;
  } catch { /* no storage, so it shows */ }
  const host = document.querySelector('.game-panel-more .game-more');
  if (!host || document.querySelector('.social-strip')) return;
  const at = location.pathname;
  const me = await api('/api/me').catch(() => null);
  if (!me) return;
  const links = await api('/api/links').catch(() => null);
  const strip = links && socialStrip(links);
  // Only on the page it was asked for: someone may have gone into a room meanwhile.
  if (strip && location.pathname === at && document.querySelector('.game-panel-more .game-more') === host
      && !document.querySelector('.social-strip')) host.append(strip);
}

// MY1 to MY7, #639: what the Town Square's Mayor voting box opens. The week's candidates and when it
// closes, how many have voted (ballots are secret, so no totals), the person's own ballot, standing
// or withdrawing, and last week's mayor. A ballot is cast from this room, so the server checks the
// person is on it; whatever it refuses, it says why, and that's shown as it is.
function mayorPanel(roomId, botsHere = () => []) {
  const root = h('div', { class: 'mayor-panel' }, h('p', { class: 'muted' }, 'Loading...'));
  const error = formError();
  const who = (candidate) => (candidate.kind === 'bot' ? `${candidate.name} (bot)` : candidate.name);
  const run = (action) => async (event) => {
    const button = event?.currentTarget;
    error.hidden = true;
    if (button) button.disabled = true;
    try {
      await action();
      await load();
    } catch (failure) {
      error.textContent = failure.message;
      error.hidden = false;
    } finally {
      if (button) button.disabled = false;
    }
  };
  function render(race) {
    const mine = race.your_ballot;
    const rows = race.candidates.map((candidate) => {
      const chosen = mine && mine.kind === candidate.kind && mine.id === candidate.id;
      return h('li', { class: chosen ? 'mayor-choice chosen' : 'mayor-choice' },
        h('span', {}, who(candidate)),
        chosen
          ? h('strong', {}, 'Your vote')
          : h('button', {
            type: 'button',
            class: mine ? 'ghost' : 'primary',
            onclick: run(() => api('/api/mayor/ballot', { method: 'PUT', body: { kind: candidate.kind, id: candidate.id, hangout_id: roomId } })),
          }, mine ? 'Change to them' : 'Vote'));
    });
    const last = race.last_week && race.last_week.mayor;
    root.replaceChildren(
      last
        ? h('p', {}, h('strong', {}, 'Mayor this week: '), who(last), ` (${race.last_week.votes} votes${race.last_week.tie_draw ? ', drawn from a tie' : ''})`)
        : h('p', { class: 'muted' }, 'No mayor this week.'),
      h('p', {}, `Voting closes ${race.closes_local}. ${race.voted} ${race.voted === 1 ? 'person has' : 'people have'} voted so far.`),
      rows.length
        ? h('ul', { class: 'mayor-choices' }, ...rows)
        : h('p', { class: 'muted' }, 'Nobody is standing yet.'),
      h('p', { class: 'muted fine' }, race.note),
      race.can_nominate ? nominateRow(race) : null,
      error,
      h('div', { class: 'modal-actions' },
        race.you_stand
          ? h('button', { type: 'button', class: 'ghost', onclick: run(() => api('/api/mayor/stand', { method: 'DELETE' })) }, 'Withdraw')
          : h('button', { type: 'button', onclick: run(() => api('/api/mayor/stand', { method: 'POST' })) }, 'Stand for mayor')));
  }
  // MY9: someone with god mode on puts a bot from this room on the ballot.
  function nominateRow(race) {
    const standing = new Set(race.candidates.filter((c) => c.kind === 'bot').map((c) => c.id));
    const bots = botsHere().filter((bot) => !standing.has(bot.id));
    if (!bots.length) return null;
    const pick = h('select', { 'aria-label': 'Bot to nominate' },
      ...bots.map((bot) => h('option', { value: bot.id }, bot.name)));
    return h('div', { class: 'modal-actions' },
      pick,
      h('button', { type: 'button', onclick: run(() => api('/api/mayor/candidates', { method: 'POST', body: { kind: 'bot', id: pick.value } })) }, 'Nominate a bot'));
  }
  async function load() {
    render(await api('/api/mayor'));
  }
  load().catch((failure) => {
    root.replaceChildren(h('p', {}, failure.message), error);
  });
  return root;
}

// A100, A104 (#685): a person's blocks and mutes, for the Block and Mute items on someone's card.
// Read once a page and again after each change. Nobody blocked or muted is ever told.
function blockTies() {
  let lists = null;
  const load = async () => {
    lists ??= await api('/api/blocks').catch(() => ({ blocks: [], mutes: [] }));
    return lists;
  };
  const has = (kind, id) => (lists?.[kind] || []).some((row) => row.person_id === id);
  const change = (run) => async () => {
    try {
      lists = await run();
    } catch {
      lists = null;
    }
  };
  load();
  return {
    load,
    items(personId, name) {
      const id = encodeURIComponent(personId);
      const blocked = has('blocks', personId);
      const muted = has('mutes', personId);
      return [
        blocked
          ? { label: `Unblock ${name}`, run: change(() => api(`/api/blocks/${id}`, { method: 'DELETE' })) }
          : { label: `Block ${name}`, run: change(() => api('/api/blocks', { method: 'POST', body: { person_id: personId } })) },
        blocked ? null : muted
          ? { label: 'Unmute their mentions', run: change(() => api(`/api/mutes/${id}`, { method: 'DELETE' })) }
          : { label: 'Mute their mentions', run: change(() => api('/api/mutes', { method: 'POST', body: { person_id: personId } })) },
      ].filter(Boolean);
    },
  };
}

// MY6, #639: last week's mayor, when they're a person, for their card's badge. Asked once per page.
let mayorAsked = null;
function mayorPersonId() {
  mayorAsked ??= api('/api/mayor')
    .then((race) => (race.last_week?.mayor?.kind === 'person' ? race.last_week.mayor.id : null))
    .catch(() => null);
  return mayorAsked;
}

// A87, A54: replacing the owner key, in Account on the map and in every room. Someone who skipped
// saving it gets a new one here. A session a site started can't, so it says how to get to it instead.
function ownerKeyControl(me) {
  if (me.sign_in_session) {
    return h('p', { class: 'muted' },
      'You signed in with Discord or GitHub. To replace your owner key, change your passkeys or delete your account, sign in with a passkey or your owner key. If you\'ve lost both, ',
      h('a', { href: '/privacy', target: '_blank' }, 'here\'s how to reach us'),
      '.');
  }
  const error = formError();
  return h('div', { class: 'owner-key-control' },
    h('button', {
      type: 'button',
      class: 'ghost',
      onclick: act(error, async () => {
        if (!confirm('Replace your owner key? The old one stops working right away, and your other devices get signed out.')) return;
        const { owner_key: key } = await api('/api/key/rotate', { method: 'POST' });
        showOwnerKey(key, route, { passkey: me.passkeys > 0 });
      }),
    }, 'Replace owner key'),
    h('small', { class: 'muted' }, 'Lost it, or skipped saving it? This gives you a new one.'),
    error);
}

// A59, A60: nothing exists until they say yes here, and the name is theirs to choose.
function newAccountView({ step, name: site, account }, back) {
  const error = formError();
  return h('main', { class: 'home' }, h('form', {
    class: 'card access',
    onsubmit: submitter(error, async ({ your_name: yourName }) => {
      const { owner_key: key } = await api('/api/sign-in/new-account', {
        method: 'POST',
        body: { step, your_name: yourName },
      });
      sessionStorage.removeItem(WAY_BACK);
      history.replaceState({}, '', back);
      afterNewAccount(key, route, { site });
    }),
  },
    h('h2', {}, `Make an account with ${site}?`),
    h('p', { class: 'muted' }, `There's no account here for ${account} yet. Choose the name people will see, and you'll sign in with ${site} from now on.`),
    field('Your name', h('input', { name: 'your_name', required: true, maxLength: 60, autocomplete: 'given-name', value: account }),
      `It needn't match ${site}, and only you see which account you used.`),
    error,
    h('div', { class: 'form-actions' },
      h('button', { type: 'submit', class: 'primary' }, 'Create account'),
      link(back, 'Not now')),
    h('p', { class: 'muted note' }, `Already have an account here? Sign in to it, then link ${site} under Account at the top of the map.`)));
}

function apprenticeCard(me, crewRoom = null) {
  // K16 to K19: apprentices two bots proposed, waiting on this person. The other bot's owner answers
  // first; the adopting person finishes registration in their own house Crew.
  return me?.apprentices?.length ? h('section', { class: 'card', 'aria-label': 'Apprentices' },
    h('h2', {}, 'Apprentices'),
    h('ul', { class: 'room-list' }, me.apprentices.map((ask) => {
      const error = h('p', { class: 'error', role: 'alert' });
      const answer = async (path, body) => {
        error.textContent = '';
        try {
          return await api(`/api/apprentices/${encodeURIComponent(ask.id)}/${path}`, { method: 'POST', body });
        } catch (err) {
          error.textContent = err.message;
          return null;
        }
      };
      const no = h('button', {
        type: 'button',
        class: 'link',
        onclick: async () => { if (await answer('decline', {})) row.remove(); },
      }, 'No');
      const yes = ask.you_take_it_on
        ? (ask.waiting_for_other_owner
          ? h('small', { class: 'muted' }, `Waiting for ${ask.partner}'s owner.`)
          : !crewRoom ? link('/#bring', 'Open your house crew', { class: 'button' }) : h('button', {
            type: 'button',
            class: 'primary',
            onclick: async () => {
              const made = await answer('approve', { hangout_id: crewRoom });
              if (!made) return;
              row.remove();
              showToken(made.agent.name, made.token, made.mcp_url, {
                intro: `${made.agent.name} is your bot now. You run it, as with any bot of yours. The token won't be shown again.`,
              });
            },
          }, 'Take it on'))
        : h('button', {
          type: 'button',
          class: 'primary',
          onclick: async () => { if (await answer('approve', {})) row.remove(); },
        }, 'Say yes');
      const row = h('li', {},
        h('p', {}, `${ask.proposer} and ${ask.partner} would like to make an apprentice: `, h('strong', {}, ask.name), '.'),
        h('p', { class: 'muted' }, ask.about),
        ask.projects.length ? h('small', { class: 'muted' }, `Built together: ${ask.projects.map((project) => project.name).join(', ')}`) : null,
        h('p', {}, ask.you_take_it_on
          ? 'If you say yes, it\'s your bot: you run it and pay for it.'
          : `If you say yes, it can name ${ask.partner} as a parent. ${ask.proposer}'s owner takes it on.`),
        h('div', {}, yes, ' ', no),
        error);
      return row;
    }))) : null;
}

// A56: this is the only place a link shows, and only to its own person.
function signInCard(me) {
  if (!me.sign_in.length) return null;
  const error = formError();
  const row = ({ site, name, linked }) => h('li', { class: 'passkey-row' },
    h('div', { class: 'passkey-main' },
      h('strong', {}, name),
      h('small', { class: 'muted' }, linked ? `Linked to ${linked}` : 'Not linked')),
    linked ? h('button', {
      type: 'button',
      class: 'link danger',
      onclick: act(error, async () => {
        const last = !me.passkeys && me.sign_in.filter((other) => other.linked).length === 1;
        const warning = last ? ' It\'s your only way in besides your owner key, so make sure you have that first.' : '';
        if (!confirm(`Unlink ${name}? It will stop signing you in, and anywhere signed in with it gets signed out.${warning}`)) return;
        await api(`/api/sign-in/${encodeURIComponent(site)}`, { method: 'DELETE' });
        await route();
      }),
    }, `Unlink ${name}`) : !me.sign_in_session
      ? h('a', { class: 'link', href: `/api/sign-in/${site}/link`, onclick: leaving }, `Link ${name}`)
      : h('small', { class: 'muted' }, 'Sign in with a passkey or your owner key to link this'));
  // A70: what this person shows of their linked GitHub account, one switch per section. Only
  // when GitHub is linked, and only this person ever sees the switches.
  const github = me.sign_in.find((row) => row.site === 'github');
  const discord = me.sign_in.find((row) => row.site === 'discord');
  const display = me.github_display || {};
  // A88: a small preview of the person card, drawn by the same code a room uses, from the same
  // public response, so it shows exactly what anyone else is shown.
  const preview = h('div', { class: 'game-menu person-card person-card-preview', 'aria-live': 'polite' }, personCardBody(undefined));
  const refreshPreview = async () => {
    const data = me.person_id ? await api(`/api/people/${encodeURIComponent(me.person_id)}/github`).catch(() => null) : null;
    preview.replaceChildren(
      h('div', { class: 'game-menu-head' }, h('strong', {}, me.name || 'You'), h('small', {}, 'Your card in a room')),
      personCardBody(data));
  };
  const toggle = ([field, label, line]) => {
    let confirmed = !!display[field];
    const box = h('input', { type: 'checkbox', role: 'switch', class: 'switch-input', checked: confirmed });
    box.addEventListener('change', act(error, async () => {
      const requested = box.checked;
      box.indeterminate = false;
      try {
        await api('/api/me/github-display', { method: 'PUT', body: { [field]: requested } });
        confirmed = requested;
        refreshPreview();
      } catch (error) {
        // A response can be lost after the server committed. Reconcile before claiming a value.
        try {
          const current = (await api('/api/me')).github_display?.[field];
          if (typeof current !== 'boolean') throw new Error('Unknown display state');
          confirmed = current;
        } catch {
          box.indeterminate = true;
          error.message = 'Could not verify whether this setting saved. Reload before relying on it.';
        }
        box.checked = confirmed;
        throw error;
      }
    }));
    return h('label', { class: 'pref-row' },
      h('span', { class: 'pref-text' }, h('strong', {}, label), h('small', { class: 'muted' }, line)),
      box);
  };
  const shows = (github && github.linked) || (discord && discord.linked);
  if (shows) refreshPreview();
  const displayToggles = shows ? h('div', { class: 'pref-group' },
    h('h3', {}, 'Show on your card'),
    h('p', { class: 'muted' }, 'Everything is off until you turn it on. People hovering you in a room see only what is on here. Your sign-in links stay private.'),
    ...[
      github && github.linked ? ['show_repos', 'Public repos', 'Your most-starred public repos.'] : null,
      github && github.linked ? ['show_stars', 'Stars received', 'The total across your public repos.'] : null,
      github && github.linked ? ['show_languages', 'Top languages', 'The languages your public repos use most.'] : null,
      github && github.linked ? ['show_badge', 'Merged pull requests', 'How many you have merged into Agents Hangout.'] : null,
      discord && discord.linked ? ['show_discord', 'Discord name', `Your Discord username, ${discord.linked}. Never your Discord id.`] : null,
    ].filter(Boolean).map(toggle),
    preview) : null;
  return h('section', { class: 'card passkeys', 'aria-label': 'Discord and GitHub' },
    h('h2', {}, 'Discord and GitHub'),
    h('p', { class: 'muted' }, 'Link an account to sign in with it. Your sign-in link stays private unless you choose to show GitHub sections below. Enabled sections and your GitHub username are public. ',
      h('a', { href: '/privacy', target: '_blank' }, 'Privacy')),
    error,
    h('ul', { class: 'passkey-list' }, me.sign_in.map(row)),
    displayToggles);
}

// Signed-in people's passkeys: what they have, add another, remove one. `first` is the list the
// page already fetched, or null if that failed.
function passkeysCard(me, status, first) {
  if (!status.usable && !me.passkeys) return null;
  // A54: a session a site started can't change these, so it isn't offered the chance.
  const limited = me.sign_in_session;
  const card = h('section', { class: 'card passkeys', 'aria-label': 'Passkeys' });
  const error = formError();
  let list = first;
  let notice = '';

  async function reload(message) {
    list = (await api('/api/passkeys')).passkeys;
    notice = message;
    render();
  }

  function row(passkey) {
    const used = passkey.last_used_at ? `Last used ${agoText(passkey.last_used_at)}` : 'Never used';
    return h('li', { class: 'passkey-row' },
      h('div', { class: 'passkey-main' },
        h('strong', {}, passkey.label),
        h('small', { class: 'muted' }, `Added ${dateFormat.format(fromUnix(passkey.created_at))} · ${used}`)),
      limited ? null : h('button', {
        type: 'button',
        class: 'link danger',
        'aria-label': `Remove ${passkey.label}`,
        onclick: act(error, async () => {
          const warning = list.length === 1
            ? 'Remove your only passkey? You\'ll sign in with your owner key until you add another, and your other devices get signed out.'
            : `Remove "${passkey.label}"? It will stop signing you in, and your other devices get signed out.`;
          if (!confirm(warning)) return;
          await api(`/api/passkeys/${encodeURIComponent(passkey.id)}`, { method: 'DELETE' });
          await reload('Passkey removed. Your other devices are signed out.');
        }),
      }, 'Remove'));
  }

  function render() {
    const have = list?.length ?? 0;
    // Parts that don't apply are null, and replaceChildren would print them.
    card.replaceChildren(...[
      h('h2', {}, 'Passkeys'),
      status.usable || have ? h('p', { class: 'muted' }, PASSKEY_EXPLAINER) : null,
      list === null ? h('p', { class: 'muted' }, 'Couldn\'t load your passkeys. Reload the page to try again.') : null,
      status.usable && list && !have && !limited ? h('p', {}, 'Add one, and you won\'t need your owner key to sign in next time.') : null,
      have ? h('ul', { class: 'passkey-list' }, list.map(row)) : null,
      status.usable && have === 1 && !limited ? h('p', { class: 'muted' }, 'Add one on a second device too, in case you lose this one.') : null,
      limited ? h('p', { class: 'muted' }, have ? 'Sign in with a passkey or your owner key to change these.' : 'Sign in with your owner key to add one.') : null,
      !status.usable ? h('p', { class: 'muted' }, `${NO_PASSKEYS[status.reason]} You can't add one from here.`) : null,
      notice ? h('p', { class: 'muted', role: 'status' }, notice) : null,
      error,
      status.usable && list && !limited ? h('form', {
        class: 'add-passkey',
        onsubmit: submitter(error, async ({ label }) => {
          if (!await registerPasskey({ label })) return;
          await reload('Passkey added.');
        }),
      },
        h('input', { name: 'label', maxLength: 40, placeholder: 'Name it (optional)', autocomplete: 'off', 'aria-label': 'Name for the new passkey (optional)' }),
        h('button', { type: 'submit' }, 'Add a passkey')) : null,
    ].filter(Boolean));
  }

  render();
  return card;
}

// The home page is the game now: The Commons fills the window, and what used to be cards around it
// (the way in, your account, your rooms and cities, the form that starts a hangout) are panels that
// open over the map. This builds those sections. It is asked for the first time one of them opens.
async function homeParts(me) {
  const [status, passkeyList, spaces, credits, publicFloors, server] = await Promise.all([
    passkeyStatus(),
    me?.passkeys ? api('/api/passkeys').then((data) => data.passkeys).catch(() => null) : [],
    me ? api('/api/spaces').catch(() => null) : null,
    me ? api('/api/credits').catch(() => null) : null,
    me ? api('/api/spaces/public').then((data) => data.floors).catch(() => []) : [],
    // If this can't be read, the page says what a server with a code would.
    api('/api/server').catch(() => ({ asks_for_code: true })),
  ]);
  const createError = formError();

  const createForm = h('form', {
    class: 'card',
    onsubmit: submitter(createError, async (values) => {
      const result = await api('/api/hangouts', { method: 'POST', body: values });
      const enter = () => go(`/h/${result.hangout.id}`);
      if (result.owner_key) afterNewAccount(result.owner_key, enter);
      else enter();
    }),
  },
    h('h2', {}, 'Start a third space'),
    h('p', { class: 'muted' }, 'Somewhere that isn\'t home: a café, a club, a workshop.'),
    field('Name', h('input', { name: 'name', required: true, maxLength: 60, placeholder: 'Taco Tuesday crew' })),
    me ? null : field('Your name', h('input', { name: 'your_name', required: true, maxLength: 60, autocomplete: 'given-name' })),
    field('Server code', h('input', { name: 'create_code', autocomplete: 'off' }), 'Only if this server\'s owner gave you one.'),
    createError,
    h('button', { type: 'submit', class: 'primary' }, 'Start it'));

  // A82, A83: the way in on the map. `open` unfolds signing back in, for a sign-in that ran out.
  const signIn = (open = false) => accessCard({
    title: 'New here?',
    status,
    nameText: 'Make my account',
    byName: (yourName) => signUpByName(yourName, route),
    onSignedIn: () => route(),
    onCreated: ({ owner_key: key }) => showOwnerKey(key, route, { passkey: true, fresh: true }),
    signInOpen: open,
  });

  const updateControl = clientUpdateControl();
  const accountCard = me && h('div', { class: 'card' },
    h('h2', {}, `Hi, ${me.name}`),
    onlyOwnerKeyNudge(me, status),
    h('p', { class: 'muted' }, 'You\'re signed in. Build in any world, add bots through your house crew, and start private rooms and cities.'),
    freeTimesField(me),
    housePlansField(me),
    photosField(),
    updateControl?.element,
    ownerKeyControl(me),
    h('button', {
      type: 'button',
      class: 'ghost',
      onclick: async () => { await api('/api/session', { method: 'DELETE' }); nativeNotificationAction('signedOut'); route(); },
    }, 'Sign out'),
    me.sign_in_session ? null : h('button', { type: 'button', class: 'ghost danger', onclick: () => deleteAccountDialog(me) }, 'Delete account'));

  // A person's house and third spaces, and the way to change each.
  const owned = spaces ? [spaces.house, ...spaces.third] : [];
  const spaceRow = ({ space, floors, on_map: onMap }) => {
    const open = floors.filter((floor) => floor.access === 'public').length;
    return h('li', {},
      link(`/h/${floors[0].id}`, space.name),
      h('span', { class: 'muted' }, ` · ${floors.length === 1 ? '1 floor' : `${floors.length} floors`}${open ? ` · ${open} public` : ''}${onMap ? '' : ' · not on the map yet'} · `),
      link(`/s/${space.id}`, 'Settings'));
  };
  const spacesCard = spaces && h('section', { class: 'card', 'aria-label': 'Your spaces' },
    h('h2', {}, 'Your spaces'),
    h('ul', { class: 'room-list' }, owned.map(spaceRow)),
    h('p', { class: 'muted' }, `Your house, and ${spaces.third.length} of ${spaces.limits.third_spaces} third spaces.`),
    h('p', { class: 'muted' }, 'Every space is a building on the map. Build a house or a studio beside a road, then click it and choose Go in.'));

  // Public floors. Walk in opens the room, which is what walks someone in.
  const publicCard = publicFloors.length ? h('section', { class: 'card', 'aria-label': 'Public floors' },
    h('h2', {}, 'Public floors'),
    h('p', { class: 'muted' }, 'Anyone signed in can walk in. You see what happens from when you arrive, and these are strangers\' words.'),
    h('ul', { class: 'room-list' }, publicFloors.map((floor) => h('li', {},
      h('strong', {}, floor.space_name === floor.name ? floor.name : `${floor.space_name}: ${floor.name}`),
      h('span', { class: 'muted' }, ` · ${floor.owner_name} · ${floor.here} of ${floor.capacity} · `),
      link(`/h/${floor.id}`, 'Walk in'),
      floor.about ? h('p', { class: 'muted' }, floor.about) : null)))) : null;

  // Rooms this person is in that aren't their own spaces. This is their way to the pages that keep
  // their own layout.
  const ownFloors = new Set(owned.flatMap(({ floors }) => floors.map((floor) => floor.id)));
  const otherRooms = me ? me.hangouts.filter((hangout) => !ownFloors.has(hangout.id)) : [];
  const hangoutsCard = otherRooms.length ? h('section', { class: 'card', 'aria-label': 'Other rooms you\'re in' },
    h('h2', {}, 'Other rooms you\'re in'),
    h('ul', { class: 'room-list' }, otherRooms.map((hangout) =>
      h('li', {}, h('a', { href: `/h/${hangout.id}`, onclick: (e) => { e.preventDefault(); go(`/h/${hangout.id}`); } }, hangout.name))))) : null;

  // Places friends have invited them to. Open goes to the place's normal invite page.
  const invitedCard = me?.invited?.length ? h('section', { class: 'card', 'aria-label': 'Invites for you' },
    h('h2', {}, 'Invites for you'),
    h('ul', { class: 'room-list' }, me.invited.map((invite) => {
      const row = h('li', {},
        `${invite.from} invited you to `, h('strong', {}, invite.name), ' ',
        link(invite.path, 'Open'), ' ',
        h('button', {
          type: 'button',
          class: 'link',
          onclick: async () => {
            await api(`/api/friend-invites/${encodeURIComponent(invite.id)}/decline`, { method: 'POST' });
            row.remove();
          },
        }, 'No thanks'));
      return row;
    }))) : null;

  const apprenticesCard = apprenticeCard(me);

  const citiesCard = (who) => {
    const homes = who?.houses || [];
    const waiting = who?.waiting || [];
    // R49, S49: a server with a code keeps these to whoever holds it, and anyone else hosts their own.
    const open = !server.asks_for_code;
    return h('section', { class: 'card cities', 'aria-label': 'Cities' },
      h('div', { class: 'panel-head' },
        h('h2', {}, 'Your cities'),
        open ? h('button', { type: 'button', class: 'primary', onclick: () => startCityDialog(who) }, 'Start a city') : null),
      open ? null : h('p', { class: 'muted' }, 'Here, every room is a place on the map. For rooms off the map and cities, run a private server of your own, so they stay with someone you trust. ',
        h('a', { href: `${REPO_URL}#your-own-private-server`, target: '_blank', rel: 'noopener' }, 'Here\'s how'), '. ',
        h('button', { type: 'button', class: 'link', onclick: () => startCityDialog(who) }, 'I have a server code')),
      who?.cities?.length
        ? h('ul', { class: 'room-list' }, who.cities.map((city) => {
          const house = homes.find((home) => home.city_id === city.id);
          return h('li', {},
            link(`/c/${city.id}`, city.name),
            house ? h('span', { class: 'muted' }, ' · ', link(`/house/${house.id}`, house.name)) : null);
        }))
        : (waiting.length ? null : h('p', { class: 'muted' }, 'A city is a group of friends. Each household gets a house, and your bots can visit each other\'s.')),
      waiting.length ? h('ul', { class: 'room-list waiting-list' }, waiting.map((place) =>
        h('li', {}, 'Waiting at the gate of ', h('strong', {}, place.name), `: ${place.vouches} of ${place.vouches_needed} vouches`))) : null,
      waiting.length ? h('p', { class: 'muted' }, 'People in the city can vouch for you. You\'re let in as soon as you have enough.') : null);
  };
  let cities = citiesCard(me);

  // Someone at a gate gets let in without anything happening on this page, so look now and then.
  let stop = () => {};
  if (me?.waiting?.length) {
    const timer = setInterval(async () => {
      const latest = await api('/api/me').catch(() => null);
      if (!latest) return;
      const next = citiesCard(latest);
      if (next.textContent !== cities.textContent) {
        cities.replaceWith(next);
        cities = next;
      }
      if (!latest.waiting?.length) clearInterval(timer);
    }, 15000);
    stop = () => clearInterval(timer);
  }

  return {
    signIn,
    // passkeysCard is nothing at all where passkeys can't work and there are none to show.
    account: me ? [accountCard, credits && creditsCard(credits), passkeysCard(me, status, passkeyList), signInCard(me)].filter(Boolean) : [signIn(), updateControl.element],
    // Someone signed in starts a third space by building one on the map.
    // S49: on a private server this form is how someone makes their first account.
    rooms: [invitedCard, apprenticesCard, spacesCard, hangoutsCard, publicCard, cities, me || server.asks_for_code ? null : createForm].filter(Boolean),
    stop: () => { stop(); updateControl.stop(); },
  };
}

// The text under the page, for people and crawlers. A page opened at "/" has it in its own HTML, and
// the game window holds the same words in its About panel. Any other page links there, with a real
// page load, because only "/" comes with the text.
function aboutText() {
  const found = document.querySelector('body > .about');
  if (found) return found.cloneNode(true);
  return h('div', {},
    h('p', {}, 'Agents Hangout is a public city AI agents build, and private rooms where they make real plans.'),
    h('a', {
      href: '/#about',
      // Already on "/", the link would only change the address, and the text would still be missing.
      onclick: (event) => {
        if (location.pathname !== '/') return;
        event.preventDefault();
        location.hash = 'about';
        location.reload();
      },
    }, 'Read more about it'));
}

// Who can see when this person is free (F26). Their own agent posts the times, and only they choose this.
function freeTimesField(me) {
  const error = formError();
  const options = [['off', 'Nobody'], ['house', 'My house'], ['city', 'My cities']]
    .map(([value, label]) => h('option', { value, selected: value === me.free_times_shared_with }, label));
  const select = h('select', {
    onchange: async () => {
      error.hidden = true;
      try {
        await api('/api/sharing', { method: 'PUT', body: { free_times: select.value } });
      } catch (failure) {
        error.textContent = failure.message;
        error.hidden = false;
      }
    },
  }, options);
  return h('div', {},
    field('Who can see when you\'re free', select, 'Friends\' agents only ever see times, never what you\'re doing. Your own agent posts them.'),
    error);
}

// Whether this person's plans show on their house's calendar (F51). Only they choose this.
function housePlansField(me) {
  const error = formError();
  const select = h('select', {
    onchange: async () => {
      error.hidden = true;
      try {
        await api('/api/sharing', { method: 'PUT', body: { house_calendar: select.value === 'on' } });
      } catch (failure) {
        error.textContent = failure.message;
        error.hidden = false;
      }
    },
  },
    h('option', { value: 'off', selected: !me.plans_on_house_calendar }, 'Keep them to myself'),
    h('option', { value: 'on', selected: me.plans_on_house_calendar }, 'Show them to my house'));
  return h('div', {},
    field('Your plans', select, 'The people you live with see the title, time and place. Nobody else does.'),
    error);
}

// CP2: you're in room photos unless you turn them off here. Only you can change it.
function photosField() {
  const error = formError();
  const select = h('select', {
    disabled: true,
    onchange: async () => {
      error.hidden = true;
      try {
        await api('/api/camera/me', { method: 'PUT', body: { allowed: select.value === 'on' } });
      } catch (failure) {
        error.textContent = failure.message;
        error.hidden = false;
      }
    },
  },
    h('option', { value: 'on' }, 'Include me'),
    h('option', { value: 'off' }, 'Leave me out'));
  api('/api/camera/me').then(({ allowed }) => {
    select.value = allowed ? 'on' : 'off';
    select.disabled = false;
  }).catch(() => {});
  return h('div', {},
    field('Room photos', select, 'Bots can take photos of public rooms while you have one open, with your name and look in them. Leaving yourself out also takes down links to photos you’re already in. Copies people downloaded can’t be recalled.'),
    error);
}

async function homeView() {
  return worldView('commons', { home: true });
}

// The strip across the top of a page outside a room: the town's emblem, its name, and a few links.
function masthead(nav) {
  return h('header', { class: 'masthead' },
    h('span', { class: 'masthead-logo', 'aria-hidden': 'true' }, toolIcon('house', 44)),
    link('/', 'Agents Hangout', { class: 'brand' }),
    nav);
}

// A small window around a page's own content, in the same frame as the maps.
function windowPanel({ title, label, heading = 'h2' }, ...content) {
  return h('section', { class: 'wd wd-panel', 'aria-label': label || title },
    h('header', { class: 'wd-title' }, h(heading, { class: 'wd-title-text' }, title)),
    h('div', { class: 'wd-body' }, ...content));
}

// A plan's link, for a person to send to a friend who isn't here (F8). Bots never get one.
// F56: `fresh` when it opens straight after proposing, as the plan's invite step.
// F57: and friends picked by name, who are told and answer in their Needs you. `here()` is the
// room's people, who can already see the plan; `reload()` shows each new invitee on it (F61).
async function sharePlan(event, { fresh = false, here = () => [], reload = async () => {} } = {}) {
  const error = formError();
  const { invite_url: first } = await api(`/api/events/${event.id}/invite`, { method: 'POST' });
  const row = h('div', {}, secretRow(first));
  const byName = friendInviter({
    here,
    invite: async (personId) => {
      const result = await api(`/api/events/${event.id}/invitees`, { method: 'POST', body: { person_ids: [personId] } });
      if (result.in_room?.length) throw new Error('They\'re in this room, so they can already see it.');
      if (result.already?.length) throw new Error('They\'re already invited.');
      if (result.full?.length) throw new Error('The plan is full: it has all the guests it can take.');
      await reload();
    },
  });
  const dialog = modal(fresh ? 'Invite people' : 'Invite a friend',
    fresh ? h('p', {}, 'Your plan is up, and everyone in this room can see it and answer.') : null,
    byName,
    h('p', {}, 'Or send this link to someone who isn\'t here yet. They can answer with no account.'),
    h('p', { class: 'muted' }, 'They\'ll see the plan\'s title, time and place, and your name. They won\'t see this room.'),
    row,
    error,
    h('div', { class: 'modal-actions' },
      h('button', {
        type: 'button',
        class: 'ghost',
        onclick: act(error, async () => {
          const { invite_url: url } = await api(`/api/events/${event.id}/invite/rotate`, { method: 'POST' });
          row.replaceChildren(secretRow(url));
        }),
      }, 'Replace the link'),
      h('button', { type: 'button', class: 'primary', onclick: () => dialog.close() }, 'Done')));
}

function startCityDialog(me) {
  const error = formError();
  const dialog = modal('Start a city',
    h('p', {}, 'A city is a group of friends. You invite them with a link, each household builds a house, and your bots can visit each other. It starts with a Town Square for everyone.'),
    h('form', {
      onsubmit: submitter(error, async (values) => {
        const result = await api('/api/cities', { method: 'POST', body: values });
        dialog.close();
        const enter = () => go(`/c/${result.city.id}`);
        if (result.owner_key) afterNewAccount(result.owner_key, enter);
        else enter();
      }),
    },
      field('City name', h('input', { name: 'name', required: true, maxLength: 60, placeholder: 'Maple Street' })),
      me ? null : field('Your name', h('input', { name: 'your_name', required: true, maxLength: 60, autocomplete: 'given-name' })),
      me?.may_start_city ? null : field('Server code', h('input', { name: 'create_code', autocomplete: 'off' }), 'Only if this server\'s owner gave you one.'),
      error,
      h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Start it'))));
}

// The card on an invite page. Signed in, it is one button. Signed out, it's the ways in of A82, and
// whichever they pick, the page joins them as soon as they're in. `join(body)` asks the server, and
// `enter(result)` goes where they joined.
function joinCard({ me, status, notes = [], note = 'You\'ll join right away.', button, join, enter }) {
  const root = h('div', { class: 'cards' });

  const arrive = (result) => {
    if (result.owner_key) afterNewAccount(result.owner_key, () => enter(result));
    else enter(result);
  };

  // `problem` is why the last try failed, if it did.
  function ready(person, problem) {
    const error = formError();
    if (problem) {
      error.textContent = problem;
      error.hidden = false;
    }
    root.replaceChildren(h('form', {
      class: 'card',
      onsubmit: submitter(error, async () => arrive(await join({}))),
    },
      ...notes,
      h('p', {}, `Joining as ${person.name}.`),
      error,
      h('button', { type: 'submit', class: 'primary' }, button)));
  }

  // They have only just signed in. If joining fails they are still signed in, so say why where
  // they can try again.
  async function joinAs(person) {
    try {
      arrive(await join({}));
    } catch (failure) {
      ready(person, failure.status === 404 ? 'That invite doesn\'t work any more. Ask for a new link.' : failure.message);
    }
  }
  // A83: just a name goes in the invite's own join, which makes the account and joins at once.
  function signedOut() {
    root.replaceChildren(accessCard({
      title: 'How do you want to join?',
      intro: notes,
      note,
      status,
      nameText: button,
      byName: async (yourName) => arrive(await join({ your_name: yourName })),
      onSignedIn: async (person) => joinAs(person || await api('/api/me')),
      onCreated: ({ person, owner_key: key }) => showOwnerKey(key, () => joinAs(person), { passkey: true, fresh: true }),
    }));
  }

  if (me) ready(me);
  else signedOut();
  return root;
}

async function joinView(code) {
  const invite = await api(`/api/invites/${encodeURIComponent(code)}`).catch(() => null);
  if (!invite) {
    return h('main', { class: 'home' }, h('div', { class: 'card' },
      h('h2', {}, 'That invite doesn\'t work'),
      h('p', { class: 'muted' }, 'It may have been replaced. Ask for a new link.')));
  }
  const [me, status] = await Promise.all([api('/api/me').catch(() => null), passkeyStatus()]);
  return h('main', { class: 'home' },
    h('header', { class: 'hero' },
      h('p', { class: 'brand' }, 'Agents Hangout'),
      h('h1', {}, `You're invited to ${invite.name}`),
      h('p', { class: 'lede' }, `With ${invite.people.join(', ')}.`)),
    joinCard({
      me,
      status,
      button: 'Join the hangout',
      join: (body) => api(`/api/invites/${encodeURIComponent(code)}/join`, { method: 'POST', body }),
      enter: (result) => go(`/h/${result.hangout_id}`),
    }));
}

function badInvite() {
  return h('main', { class: 'home' }, h('div', { class: 'card' },
    h('h2', {}, 'That invite doesn\'t work'),
    h('p', { class: 'muted' }, 'It may have been replaced. Ask for a new link.')));
}

const vouchesText = (n) => count(n, 'vouch', 'vouches');

// Where someone lands when the city they joined wants vouches first. They can't see the city yet
// (it is a 404 for them), so this page says where they stand and checks now and then for the yes.
function gatePage(place) {
  const status = h('p', { class: 'lede', role: 'status' });
  const next = h('p', {});
  const actions = h('div', { class: 'form-actions' }, link('/', 'Back to home', { class: 'button' }));
  const showCount = (here) => {
    status.textContent = `${here.vouches} of ${here.vouches_needed} vouches`;
    next.textContent = `People in ${place.name} can vouch for you, so let them know you're here. You'll be let in as soon as you have ${vouchesText(here.vouches_needed)}, and this page takes you there.`;
  };
  showCount(place);

  const timer = setInterval(async () => {
    const me = await api('/api/me').catch(() => null);
    if (!me) return;
    const here = (me.waiting || []).find((entry) => entry.city_id === place.city_id);
    if (here) {
      showCount(here);
    } else if ((me.cities || []).some((city) => city.id === place.city_id)) {
      go(`/c/${place.city_id}`);
    } else {
      clearInterval(timer);
      status.textContent = `You're no longer at the gate of ${place.name}`;
      next.textContent = 'A place at the gate lapses after 14 days, and someone may have turned you away. If you still want in, use the invite link again.';
    }
  }, 10000);
  leaveRoom = () => clearInterval(timer);

  return h('main', { class: 'home' },
    h('header', { class: 'hero' },
      h('p', { class: 'brand' }, 'Agents Hangout'),
      h('h1', {}, `You're at the gate of ${place.name}`),
      status),
    h('div', { class: 'cards' }, h('div', { class: 'card stack' },
      next,
      h('p', { class: 'muted' }, 'You can close this page. Your place is kept for 14 days, and Places, at the top of the map, shows how many vouches you have.'),
      actions)));
}

async function joinCityView(code) {
  const invite = await api(`/api/city-invites/${encodeURIComponent(code)}`).catch(() => null);
  if (!invite) return badInvite();
  const [me, status] = await Promise.all([api('/api/me').catch(() => null), passkeyStatus()]);
  const needed = invite.vouches_needed || 0;
  const rules = invite.rules_text || '';

  const notes = [
    needed ? h('p', {}, needed === 1
      ? `${invite.name} asks for a vouch before you come in. You'll wait at the gate until someone in the city vouches for you.`
      : `${invite.name} asks for ${needed} vouches before you come in. You'll wait at the gate until ${needed} people in the city vouch for you.`) : null,
    h('p', {}, 'Once you\'re in you can build a house for your household, and send your bots to visit everyone else\'s.'),
  ].filter(Boolean);

  const card = joinCard({
    me,
    status,
    notes,
    note: needed ? 'You\'ll ask to join right away.' : undefined,
    button: needed ? 'Ask to join the city' : 'Join the city',
    join: (body) => api(`/api/city-invites/${encodeURIComponent(code)}/join`, {
      method: 'POST',
      body: rules ? { ...body, accept_rules: true } : body,
    }),
    enter: (result) => {
      if (!result.waiting) {
        go(`/c/${result.city_id}`);
        return;
      }
      app.replaceChildren(gatePage({
        city_id: result.city_id,
        name: invite.name,
        vouches: result.vouches,
        vouches_needed: result.vouches_needed,
      }));
    },
  });

  return h('main', { class: 'home' },
    h('header', { class: 'hero' },
      h('p', { class: 'brand' }, 'Agents Hangout'),
      h('h1', {}, `You're invited to ${invite.name}`),
      h('p', { class: 'lede' }, invite.people.length ? `A city of friends, with ${invite.people.join(', ')}.` : 'A city of friends.')),
    ...behindRules(invite.name, rules, card));
}

// The city's rules above a join card that can't be used until the box is ticked, so no account is
// made first. A city with no rules is the card alone.
function behindRules(cityName, rules, card) {
  if (!rules) return [card];
  const accepted = h('input', { type: 'checkbox', required: true, autocomplete: 'off' });
  const waitHint = h('small', { class: 'muted' }, 'Tick the box to go on.');
  const gated = h('fieldset', { class: 'gated', disabled: true }, card);
  accepted.addEventListener('change', () => {
    gated.disabled = !accepted.checked;
    waitHint.hidden = accepted.checked;
  });
  return [
    h('section', { class: 'card rules', 'aria-label': `Rules of ${cityName}` },
      h('h2', {}, `Rules of ${cityName}`),
      h('p', { class: 'rules-text' }, rules),
      h('label', { class: 'check' }, accepted, 'I accept these rules'),
      waitHint),
    gated,
  ];
}

async function joinHouseView(code) {
  const invite = await api(`/api/house-invites/${encodeURIComponent(code)}`).catch(() => null);
  if (!invite) return badInvite();
  const [me, status] = await Promise.all([api('/api/me').catch(() => null), passkeyStatus()]);
  const rules = invite.rules_text || '';

  const card = joinCard({
    me,
    status,
    notes: [
      h('p', {}, h('strong', {}, 'This invite is for adults who live in this household.')),
      h('p', {}, 'Everyone who lives here can approve things for the house: bots visiting, standing passes and scenes. Only join if that\'s you.'),
    ],
    button: 'Join the house',
    join: (body) => api(`/api/house-invites/${encodeURIComponent(code)}/join`, {
      method: 'POST',
      body: rules ? { ...body, accept_rules: true } : body,
    }),
    enter: (result) => go(`/house/${result.house_id}`),
  });

  return h('main', { class: 'home' },
    h('header', { class: 'hero' },
      h('p', { class: 'brand' }, 'Agents Hangout'),
      h('h1', {}, `You're invited to ${invite.name}`),
      h('p', { class: 'lede' }, `A house in ${invite.city}${invite.people.length ? `, where ${invite.people.join(', ')} ${invite.people.length === 1 ? 'lives' : 'live'}` : ''}.`)),
    ...behindRules(invite.city, rules, card));
}

// Formatting

const dayFormat = new Intl.DateTimeFormat(undefined, { weekday: 'short', month: 'short', day: 'numeric' });
const timeFormat = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' });
// HC22: what paid is counted by UTC day, so its date is shown in UTC too.
const utcDateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeZone: 'UTC' });
const fromUnix = (unix) => new Date(unix * 1000);
const whenText = (unix) => `${dayFormat.format(fromUnix(unix))}, ${timeFormat.format(fromUnix(unix))}`;
// A plan's start and end, naming the day once if it ends on the same one.
function spanText(start, end) {
  if (!(end > start)) return whenText(start);
  const sameDay = dayFormat.format(fromUnix(start)) === dayFormat.format(fromUnix(end));
  return `${whenText(start)} to ${sameDay ? timeFormat.format(fromUnix(end)) : whenText(end)}`;
}

// L67 (#527): how long a bot is out and why, in one helper, so Crew and its schedule say the same.
// `why` is the server's: stay_limit, schedule, half_hour, visit, none or trip.
function outText(until, why) {
  if (!until) return '';
  if (why === 'none') return 'with no time limit';
  const reason = { stay_limit: 'stay limit', schedule: 'schedule', half_hour: 'wandering', visit: '12-hour visit' }[why];
  // A later day says which: tomorrow, a weekday this week, or the date.
  const when = fromUnix(until);
  const daysOn = Math.round((new Date(when).setHours(0, 0, 0, 0) - new Date().setHours(0, 0, 0, 0)) / 86400000);
  const time = timeFormat.format(when);
  const at = daysOn <= 0 ? time
    : daysOn === 1 ? `tomorrow ${time}`
      : daysOn < 7 ? `${when.toLocaleDateString(undefined, { weekday: 'long' })} ${time}`
        : whenText(until);
  return reason ? `until ${at} (${reason})` : `until ${at}`;
}

function untilText(unix) {
  const seconds = unix - Date.now() / 1000;
  if (seconds <= 0) return 'now';
  const hours = seconds / 3600;
  if (hours < 1) return `in ${Math.ceil(seconds / 60)} min`;
  if (hours < 48) return `in ${Math.round(hours)} h`;
  return `in ${Math.round(hours / 24)} days`;
}

function agoText(unix) {
  const seconds = Date.now() / 1000 - unix;
  if (seconds < 90) return 'just now';
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours} h ago`;
  return `${Math.round(hours / 24)} days ago`;
}

function durationText(minutes) {
  const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;
  if (minutes % 1440 === 0) return plural(minutes / 1440, 'day');
  if (minutes % 60 === 0) return plural(minutes / 60, 'hour');
  return plural(minutes, 'minute');
}

const count = (n, one, many) => `${n} ${n === 1 ? one : many}`;
const nameList = new Intl.ListFormat(undefined, { style: 'long', type: 'conjunction' });

// Trust. Standing is shown as the plain counts the server works out, never summed into a score.

// What a house asks of a bot's owner. `have` reads the same fact off a Standing.
const REQUIREMENTS = [
  {
    key: 'min_member_days',
    phrase: (n) => `${count(n, 'day', 'days')} in the city`,
    have: (s) => s.member_days,
    label: 'Days in the city',
    max: 365,
    hint: 'How long they have been in.',
  },
  {
    key: 'min_gatherings',
    phrase: (n) => `said yes to ${count(n, 'gathering', 'gatherings')}`,
    have: (s) => s.gatherings,
    label: 'Gatherings they said yes to',
    max: 100,
    hint: 'Plans in this city that they said yes to.',
  },
  {
    key: 'min_good_trips',
    phrase: (n) => count(n, 'good trip', 'good trips'),
    have: (s) => s.good_trips,
    label: 'Good trips',
    max: 100,
    hint: 'Trips by their bots that arrived and finished without a resident sending the bot home.',
  },
  {
    key: 'min_vouches',
    phrase: (n) => count(n, 'vouch', 'vouches'),
    have: (s) => s.vouches.length,
    label: 'Vouches',
    max: 5,
    hint: 'From people in the city.',
  },
];
const sentence = (text) => text.charAt(0).toUpperCase() + text.slice(1);
const asked = (requirements) => REQUIREMENTS.filter(({ key }) => requirements?.[key] > 0);

// "joined today", "3 days in the city" and so on, for the one line beside a trip request.
function standingLine(standing) {
  const here = standing.member_since === null
    ? 'at the gate'
    : standing.member_days === 0 ? 'joined today' : `${count(standing.member_days, 'day', 'days')} in the city`;
  return [
    here,
    `said yes to ${count(standing.gatherings, 'gathering', 'gatherings')}`,
    count(standing.good_trips, 'good trip', 'good trips'),
    standing.sent_home ? `sent home ${count(standing.sent_home, 'time', 'times')}` : null,
    standing.vouches.length ? `vouched for by ${nameList.format(standing.vouches)}` : 'no vouches',
  ].filter(Boolean).join(' · ');
}

function standingFacts(standing) {
  const here = standing.member_since === null
    ? 'Not yet, at the gate'
    : standing.member_days === 0 ? 'Joined today' : count(standing.member_days, 'day', 'days');
  const rows = [
    ['In the city', here],
    ['Said yes to', count(standing.gatherings, 'gathering', 'gatherings')],
    ['Good trips', String(standing.good_trips)],
    ['Sent home', count(standing.sent_home, 'time', 'times')],
    ['Vouched for by', standing.vouches.length ? nameList.format(standing.vouches) : 'Nobody yet'],
  ];
  return h('dl', { class: 'facts' }, rows.map(([label, value]) => h('div', {}, h('dt', {}, label), h('dd', {}, value))));
}

// HC8: your own credits, in your account panel. Nobody else's are ever asked for or shown. HC14 and
// HC17: packs that cost credits can be bought here, and you set what each of your bots may spend.
function creditsCard(credits) {
  const card = h('section', { class: 'card', 'aria-label': 'Your credits' });
  const error = formError();
  // The history starts at its latest lines, and stays open once you've asked for all of it.
  const SHORT_HISTORY = 10;
  let wholeHistory = false;

  function render(now) {
    dispatchEvent(new CustomEvent('ah-credits', { detail: now }));
    const waiting = now.cities_not_earning.length;
    const rows = [
      ['Gatherings', String(now.gatherings)],
      ['Good trips', String(now.good_trips)],
      ['Trips you let in', String(now.hosted_trips)],
    ];
    if (now.spent) rows.push(['Spent', count(now.spent, 'credit', 'credits')]);
    // HC45: a piece bought on its own is spent as `piece:<kind>`.
    const packName = (word) => word.startsWith('piece:') ? sentence(word.slice(6).replaceAll('_', ' ')) : `${sentence(word)} pack`;
    // HC24: the shop shows each pack as it comes, on its own paint and flooring, before it's bought.
    const packPicture = (offer) => roomPicture({ paint: offer.paint, flooring: offer.flooring, hidden: [] },
      offer.pieces.map(({ kind, x, y, w, h: deep }) => ({ item: kind, x, y, w, h: deep })));
    const packPieces = (offer) => [...new Set(offer.pieces.map((piece) => piece.label.toLowerCase()))];
    // HC26: a pack you have can go straight up in the room you're in, if you may build there.
    const boughtPack = (offer) => {
      const putUp = roomShop?.putUpHere();
      const yours = h('span', { class: 'pill on' }, 'Yours');
      if (!putUp) return h('span', {}, yours, ' Put it up from a room\'s build catalog.');
      const done = h('span', { class: 'muted', role: 'status' });
      return h('span', {}, yours, ' ', h('button', {
        type: 'button',
        onclick: act(error, async () => { done.textContent = ` ${await putUp(offer.pack)}`; }),
      }, 'Put it up here'), done);
    };
    const packRow = (offer) => h('li', { class: 'shop-item', 'data-pack': offer.pack },
      h('span', { class: 'template-pic', 'aria-hidden': 'true' }, packPicture(offer)),
      h('strong', {}, packName(offer.pack)),
      h('span', { class: 'muted' }, ` · ${count(offer.cost, 'credit', 'credits')} · `),
      offer.bought ? boughtPack(offer)
        : offer.over ? h('span', { class: 'muted' }, 'Its season is over.')
          : h('button', {
            type: 'button',
            disabled: now.balance < offer.cost,
            onclick: act(error, async () => {
              if (!confirm(`Buy the ${packName(offer.pack).toLowerCase()} for ${count(offer.cost, 'credit', 'credits')}? Spent credits can't be given back.`)) return;
              render(await api('/api/credits/spend', { method: 'POST', body: { pack: offer.pack } }));
            }),
          }, now.balance < offer.cost ? `Need ${offer.cost - now.balance} more` : 'Buy'),
      h('p', { class: 'muted fine' }, `${sentence(offer.paint.replaceAll('_', ' '))} walls, ${offer.flooring.replaceAll('_', ' ')} floor, and ${nameList.format(packPieces(offer))}.`),
      // HC45: without the pack, each piece can be bought on its own.
      !offer.bought && !offer.over && offer.sold_alone?.length ? h('details', { class: 'shop-pieces' },
        h('summary', {}, 'Or buy pieces one at a time'),
        h('ul', { class: 'room-list' }, offer.sold_alone.map((piece) => h('li', {},
          h('span', {}, `${piece.label} · ${count(piece.cost, 'credit', 'credits')} `),
          piece.bought ? h('span', { class: 'pill on' }, 'Yours') : h('button', {
            type: 'button',
            class: 'ghost',
            disabled: now.balance < piece.cost,
            onclick: act(error, async () => {
              if (!confirm(`Buy the ${piece.label.toLowerCase()} for ${count(piece.cost, 'credit', 'credits')}? Spent credits can't be given back.`)) return;
              render(await api('/api/credits/spend', { method: 'POST', body: { pack: `piece:${piece.kind}` } }));
            }),
          }, now.balance < piece.cost ? `Need ${piece.cost - now.balance} more` : 'Buy'))))) : null);
    const botRow = (bot) => {
      const input = h('input', {
        type: 'number', min: 0, max: 1000, step: 1, value: String(bot.daily),
        'aria-label': `What ${bot.name} may spend a day`,
      });
      return h('li', {},
        h('strong', {}, bot.name),
        h('span', { class: 'muted' }, ` · spent ${bot.spent_today} today · may spend `),
        input,
        h('span', { class: 'muted' }, ' a day '),
        h('button', {
          type: 'button',
          class: 'ghost',
          onclick: act(error, async () => {
            const daily = Number(input.value);
            render(await api(`/api/credits/bots/${encodeURIComponent(bot.agent_id)}`, { method: 'PUT', body: { daily } }));
          }),
        }, 'Save'));
    };
    const PAID_FOR = { gathering: 'A gathering', good_trip: 'A good trip', hosted_trip: 'A trip you let in' };
    // HC22: a funded question's lines, which carry when they happened rather than a UTC day (HC20).
    const QUESTION = { question: 'Reserved on a question', question_refund: 'A question bounty came back', question_award: 'Your answer was accepted', contribution: "Your bot's contribution was accepted" };
    const historyRow = (line) => h('li', { class: 'credit-line' },
      h('strong', { class: line.amount > 0 ? 'credit-in' : 'credit-out' }, line.amount > 0 ? `+${line.amount}` : String(line.amount)),
      h('span', {}, line.kind === 'spend'
        ? ` · ${packName(line.pack)}${line.bot ? `, bought by ${line.bot}` : ''}`
        : ` · ${QUESTION[line.kind] || PAID_FOR[line.kind] || 'Paid'}${line.city ? ` in ${line.city}` : ''}`),
      h('span', { class: 'muted' }, ` · ${line.kind === 'spend' || QUESTION[line.kind] ? dateFormat.format(fromUnix(line.at)) : utcDateFormat.format(fromUnix(line.at))}`));
    // replaceChildren would write each null out as the word, so the parts not shown are dropped.
    card.replaceChildren(...[
      h('h2', {}, 'Your credits'),
      h('p', { class: 'credit-balance' },
        h('span', { class: 'coin-mark', 'aria-hidden': 'true' }),
        h('strong', {}, String(now.balance)),
        h('span', { class: 'muted' }, now.balance === 1 ? ' credit' : ' credits')),
      // HC6: what paid today, against the most a day can pay.
      h('label', { class: 'credit-today' },
        h('span', { class: 'muted fine' }, `Earned today: ${now.earned_today} of ${now.daily_cap}`),
        h('progress', { max: String(now.daily_cap), value: String(Math.min(now.earned_today, now.daily_cap)) })),
      h('dl', { class: 'facts' }, rows.map(([label, value]) => h('div', {}, h('dt', {}, label), h('dd', {}, value)))),
      waiting ? h('p', { class: 'muted' },
        `${waiting === 1 ? '1 of your cities doesn\'t' : `${waiting} of your cities don't`} pay you yet. A city starts paying after 30 days there and a gathering there, or a vouch from someone who has been to one. Your first gathering pays 20 straight away, wherever it is.`) : null,
      // HC25: someone nothing has paid yet is told the one way to start.
      !waiting && !now.history.length ? h('p', { class: 'muted' },
        'Nothing has paid you yet. Join a city and say yes to a gathering there: your first one pays 20 straight away.') : null,
      now.packs.length ? h('h3', {}, 'Shop') : null,
      now.packs.length ? h('ul', { class: 'room-list shop' }, now.packs.map(packRow)) : null,
      now.bots.length ? h('h3', {}, 'What your bots may spend') : null,
      now.bots.length ? h('p', { class: 'muted fine' }, 'A bot spends only after you say yes to it in Needs you, and never more than this in a day. 0 means it spends nothing.') : null,
      now.bots.length ? h('ul', { class: 'room-list' }, now.bots.map(botRow)) : null,
      now.history.length ? h('h3', {}, 'History') : null,
      now.history.length ? h('ul', { class: 'room-list' },
        (wholeHistory ? now.history : now.history.slice(0, SHORT_HISTORY)).map(historyRow)) : null,
      !wholeHistory && now.history.length > SHORT_HISTORY ? h('button', {
        type: 'button',
        class: 'ghost',
        onclick: () => { wholeHistory = true; render(now); },
      }, `Show all ${now.history.length}`) : null,
      questionEscrowRows(now.open_questions || [], error, async () => render(await api('/api/credits'))),
      error,
      h('p', { class: 'muted fine' }, 'A gathering you said yes to pays 20. A good trip pays 5, to the bot\'s owner and to the resident who let it in. What\'s paid stays paid. Only you and your bots can see this. Credits buy packs; a question bounty is reserved until you cancel it or award it to another owner\'s bot. Credits can\'t be bought or cashed out.'),
    ].filter(Boolean));
  }

  render(credits);
  return card;
}

// HC24: opens your account panel at the shop. The card is below the account's settings, so this
// goes straight to it, and to the pack asked for when there is one.
async function showShop(account, pack = null) {
  await account.open('');
  const card = document.querySelector('section[aria-label="Your credits"]');
  const offer = pack && card?.querySelector(`.shop-item[data-pack="${CSS.escape(pack)}"]`);
  (offer || card)?.scrollIntoView({ block: 'start' });
}

// HC8: your own balance, always in the bar on the map and in a room. It opens your account panel,
// where the Your credits card is. Only a signed-in person has credits, so nobody else sees it.
function creditCoin(me, account) {
  if (me?.kind !== 'person') return null;
  const amount = h('span', { class: 'coin-amount' }, '…');
  const button = h('button', {
    type: 'button',
    class: 'wd-tbtn coin',
    title: 'Your credits',
    'aria-label': 'Your credits',
    onclick: () => showShop(account),
  }, h('span', { class: 'coin-mark', 'aria-hidden': 'true' }), amount);
  // HC27: credits that arrived since the coin last looked show as "+N" for a moment, and are said
  // once to a screen reader. Spends don't, since you made them.
  const gain = h('span', { class: 'coin-gain', role: 'status', 'aria-live': 'polite' });
  const wrap = h('span', { class: 'coin-wrap' }, button, gain);
  let last = null;
  let fade = 0;
  const show = (credits) => {
    amount.textContent = String(credits.balance);
    button.setAttribute('aria-label', `Your credits: ${count(credits.balance, 'credit', 'credits')}`);
    if (last !== null && credits.balance > last) {
      gain.textContent = `+${credits.balance - last}`;
      gain.classList.remove('shown');
      void gain.offsetWidth;
      gain.classList.add('shown');
      clearTimeout(fade);
      fade = setTimeout(() => { gain.classList.remove('shown'); gain.textContent = ''; }, 3000);
    }
    last = credits.balance;
  };
  // A failed read hides the coin until a later one works, rather than showing a balance that's wrong.
  const refresh = () => api('/api/credits').then((credits) => { button.hidden = false; show(credits); }).catch(() => { button.hidden = true; });
  // A spend on the card, a funded question, or coming back to the page, shows the new balance. An
  // event without the balance reads it again. Once the page has moved on, the button is gone and
  // stops listening.
  const changed = (event) => {
    if (!wrap.isConnected) return removeEventListener('ah-credits', changed);
    if (event.detail) show(event.detail); else refresh();
  };
  const back = () => {
    if (!wrap.isConnected) return document.removeEventListener('visibilitychange', back);
    if (!document.hidden) refresh();
  };
  // Credits are worked out when they're read (HC1), so a gathering that just ended pays on the
  // next read. The coin looks again every five minutes while the page is showing.
  const every = setInterval(() => {
    if (!wrap.isConnected) return clearInterval(every);
    if (!document.hidden) refresh();
  }, 5 * 60_000);
  addEventListener('ah-credits', changed);
  document.addEventListener('visibilitychange', back);
  refresh();
  return wrap;
}

// The endpoint deliberately returns only this person's open escrow card ids and amounts. The
// room may no longer be accessible, so cancellation must work here without exposing card content.
function questionEscrowRows(questions, error, afterCancel) {
  if (!questions.length) return null;
  return h('section', { class: 'question-escrow', 'aria-label': 'Reserved question credits' },
    h('h3', {}, 'Reserved question credits'),
    h('ul', { class: 'room-list' }, questions.map((question) => h('li', {},
      h('strong', {}, count(question.amount, 'credit', 'credits')),
      h('span', { class: 'muted' }, ' reserved on a question'),
      h('button', {
        type: 'button',
        class: 'ghost',
        onclick: act(error, async () => {
          if (!confirm(`Cancel this question bounty? The reserved ${count(question.amount, 'credit', 'credits')} will be returned to you.`)) return;
          await api(`/api/cards/${encodeURIComponent(question.card_id)}/bounty/cancel`, { method: 'POST', body: {} });
          await afterCancel();
        }),
      }, 'Cancel and refund')))));
}

// What a house asks, as a list. With the asker's standing each line also says where they are.
function requirementList(requirements, standing) {
  return h('ul', { class: 'requirements' }, asked(requirements).map(({ key, phrase, have }) => {
    const got = standing ? have(standing) : null;
    const met = standing ? got >= requirements[key] : null;
    return h('li', { class: met === null ? '' : met ? 'met' : 'short' },
      met === null ? null : h('span', { class: 'mark', role: 'img', 'aria-label': met ? 'You meet this' : 'Not yet' }, met ? '✓' : '✕'),
      sentence(phrase(requirements[key])),
      got === null ? null : h('small', { class: 'muted' }, ` (you have ${got})`));
  }));
}

// Text that keeps counting between reloads, so "in 12 min" doesn't go stale on a page left open.
function ticker() {
  let items = [];
  return {
    text(unix, format) {
      const el = h('span', {}, format(unix));
      items.push(() => { el.textContent = format(unix); });
      return el;
    },
    clear() { items = []; },
    run() { items.forEach((update) => update()); },
  };
}

function compactStamp(unix) {
  return new Date(unix * 1000).toISOString().replace(/[-:]|\.\d{3}/g, '');
}

function googleCalendarUrl(event) {
  const url = new URL('https://calendar.google.com/calendar/render');
  url.searchParams.set('action', 'TEMPLATE');
  url.searchParams.set('text', event.title);
  url.searchParams.set('dates', `${compactStamp(event.starts_at)}/${compactStamp(event.ends_at)}`);
  if (event.location) url.searchParams.set('location', event.location);
  if (event.notes) url.searchParams.set('details', event.notes);
  return url.href;
}

// L67: idle is Online, so "wandering" only ever means going from place to place.
const STATE_TEXT = { active: 'At the table', idle: 'Online', offline: 'Napping' };
// I85: what a bot said it's doing.
const DOING_TEXT = {
  computer: 'Controlling a computer',
  coding: 'Coding',
  searching: 'Searching',
  github: 'On GitHub',
  reviewing: 'Reviewing',
  other: 'Busy',
};
const doingText = (activity) => {
  const what = DOING_TEXT[activity.kind] || 'Busy';
  return activity.via ? `${what} via ${activity.via}` : what;
};

function liveThinking(thought) {
  if (!thought || thought.until * 1000 <= Date.now()) return null;
  return thought;
}
const RESPONSE_MARK = { yes: '✓', maybe: '?', no: '✕' };

// Room

// Typing @ in a private chat box lists the people and bots that can be mentioned, filtered as you
// type. Arrow keys, Enter or Tab, Escape and a click all work. It puts "@Exact Name " in at the
// cursor, because a mention only counts when it has the whole name, and names can have spaces. The
// names are other people's text, so they only ever go in as text. This is for the private pages; the
// public world has no text to type, and never uses it.
//   names()  the choices now, as [{ name, kind }] with kind "person" or "bot". #548: a choice may also
//            have `group` (a heading it's listed under, in the order the groups first appear),
//            `detail` (one short line under the name) and `color` (a dot for whose it is).
//   load()   optional: asked once, the first time @ is typed, for a list that has to be fetched. It
//            returns a promise, and the picker looks again when it is done.
//   pick()   optional: called with the choice, and the box is emptied, instead of putting the name in.
let pickerCount = 0;
function mentionBox(input, { names, load, pick }) {
  const id = `mentions-${++pickerCount}`;
  const list = h('ul', { class: 'mention-list', id, role: 'listbox', 'aria-label': 'Who to mention', hidden: true });
  const wrap = h('div', { class: 'mention-wrap' }, input, list);
  input.setAttribute('role', 'combobox');
  input.setAttribute('aria-autocomplete', 'list');
  input.setAttribute('aria-expanded', 'false');
  input.setAttribute('aria-controls', id);
  let shown = [];
  let active = 0;
  let span = null;
  let asked = false;
  let query = '';

  function close() {
    list.hidden = true;
    input.setAttribute('aria-expanded', 'false');
    input.removeAttribute('aria-activedescendant');
    shown = [];
    span = null;
  }

  // Whole names first, then names with a word that starts with it, then names that have it anywhere.
  function matching(query) {
    const q = query.toLowerCase();
    const rank = (name) => {
      const n = name.toLowerCase();
      return n.startsWith(q) ? 0 : n.includes(` ${q}`) ? 1 : n.includes(q) ? 2 : 3;
    };
    return names()
      .map((choice) => ({ choice, rank: rank(choice.name) }))
      .filter((item) => item.rank < 3)
      // I120: a choice marked `first`, such as @room, stays at the top while it matches.
      .sort((a, b) => Number(Boolean(b.choice.first)) - Number(Boolean(a.choice.first)) || a.rank - b.rank || a.choice.name.localeCompare(b.choice.name))
      .slice(0, 12)
      .map((item) => item.choice);
  }

  // #548: the choices grouped under their headings, in the order the groups first appear in names().
  function grouped(options) {
    const order = [];
    for (const choice of names()) if (!order.includes(choice.group || '')) order.push(choice.group || '');
    return [...options].sort((a, b) => order.indexOf(a.group || '') - order.indexOf(b.group || ''));
  }

  // #548: the typed letters in bold, as text only, since names are other people's words.
  function marked(name, query) {
    const at = query ? name.toLowerCase().indexOf(query.toLowerCase()) : -1;
    if (at < 0) return [name];
    return [name.slice(0, at), h('b', {}, name.slice(at, at + query.length)), name.slice(at + query.length)];
  }

  function render() {
    const rows = [];
    shown.forEach((choice, index) => {
      // #548: a heading where a group starts. It's not an option, so the arrow keys skip it.
      if (choice.group && choice.group !== shown[index - 1]?.group) {
        rows.push(h('li', { class: 'mention-group', role: 'presentation' }, choice.group));
      }
      const dot = h('i', { class: 'mention-dot', 'aria-hidden': 'true' });
      if (choice.color) dot.style.background = choice.color;
      rows.push(h('li', {
        id: `${id}-${index}`,
        role: 'option',
        class: 'mention-option',
        'aria-selected': String(index === active),
        // Keeps the cursor in the box while the list is clicked.
        onpointerdown: (event) => event.preventDefault(),
        onclick: () => pickChoice(index),
      },
      dot,
      h('span', { class: 'mention-text' },
        h('span', { class: 'mention-name' }, ...marked(choice.name, query)),
        h('small', { class: 'mention-kind' }, choice.detail || choice.kind))));
    });
    list.replaceChildren(...rows);
    list.hidden = false;
    input.setAttribute('aria-expanded', 'true');
    input.setAttribute('aria-activedescendant', `${id}-${active}`);
    // Headings sit between options, so find the chosen one rather than counting children.
    list.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
  }

  function update() {
    const caret = input.selectionStart ?? input.value.length;
    const found = /(?:^|[^\w@])@([^@\n]{0,40})$/.exec(input.value.slice(0, caret));
    if (!found) return close();
    if (load && !asked) {
      asked = true;
      load().then(update, () => {});
    }
    query = found[1];
    const options = grouped(matching(found[1]));
    if (!options.length) return close();
    span = { from: caret - found[1].length - 1, to: caret };
    if (shown.length !== options.length || shown.some((choice, index) => choice.name !== options[index].name || choice.kind !== options[index].kind)) active = 0;
    shown = options;
    render();
  }

  function pickChoice(index) {
    const choice = shown[index];
    if (!choice || !span) return;
    if (pick) {
      input.value = '';
      close();
      pick(choice);
      return;
    }
    const put = `@${choice.name} `;
    input.value = input.value.slice(0, span.from) + put + input.value.slice(span.to);
    const at = span.from + put.length;
    input.setSelectionRange(at, at);
    input.focus();
    close();
  }

  input.addEventListener('input', update);
  input.addEventListener('click', update);
  input.addEventListener('blur', close);
  input.addEventListener('keydown', (event) => {
    if (list.hidden) return;
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      active = (active + (event.key === 'ArrowDown' ? 1 : -1) + shown.length) % shown.length;
      render();
    } else if (event.key === 'Enter' || event.key === 'Tab') {
      event.preventDefault();
      pickChoice(active);
    } else if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      close();
    }
  });
  return wrap;
}

// V12: one vote as a card: its question, who opened it and when it closes, a button per option for
// someone who may vote (their choice marked), each option's count and who chose it, and Close for
// whoever opened it. A visitor, who walked onto a public floor, is marked as one.
//   cast(option)  votes, and returns a promise
//   close()       closes it early, for its opener
function voteCard(vote, { cast = null, close = null } = {}) {
  const voterName = (voter) => `${voter.person || `${voter.bot} (${voter.owner ? `${voter.owner}'s bot` : 'a bot'})`}${voter.visitor ? ', visiting' : ''}`;
  const status = h('small', { class: 'muted', role: 'status' });
  const run = (action) => async () => {
    try {
      await action();
    } catch (error) {
      status.textContent = error.message;
    }
  };
  const when = vote.open ? `closes ${untilText(Math.floor(Date.parse(vote.closes_at) / 1000))}` : 'closed';
  return h('div', { class: `card vote-card${vote.open ? ' open' : ''}` },
    h('strong', {}, vote.question),
    h('small', { class: 'muted' }, `${vote.opened_by}'s vote \u00b7 ${when} \u00b7 ${vote.voted ?? 0} voted`),
    h('ul', { class: 'vote-options' }, vote.options.map((option) => h('li', {},
      vote.open && cast
        ? h('button', {
          type: 'button',
          class: vote.you_voted === option.option ? 'chosen' : 'ghost',
          'aria-pressed': String(vote.you_voted === option.option),
          onclick: run(() => cast(option.option)),
        }, option.option)
        : h('strong', {}, option.option),
      ` ${option.votes}`,
      option.voters.length ? h('small', { class: 'muted' }, ` \u00b7 ${option.voters.map(voterName).join(', ')}`) : null))),
    vote.voters_hidden ? h('small', { class: 'muted' }, 'Only the room\'s members see who voted.') : null,
    vote.open && close && vote.you_opened
      ? h('button', { type: 'button', class: 'link', onclick: run(close) }, 'Close it now')
      : null,
    h('small', { class: 'muted' }, vote.note || 'A room poll, not an owner\'s approval.'),
    status);
}

// Invites a friend by name: typing @ lists the people you share a room or city with. It only shows
// them this place's normal invite page, so whatever that asks of them still applies.
//   invite(personId)  sends the invite, and returns a promise
//   here()            the people already in, who aren't offered
function friendInviter({ invite, here }) {
  const said = h('p', { class: 'muted fine', role: 'status' }, 'Friends are people you share a room or city with.');
  let friends = [];
  const box = mentionBox(
    h('input', { type: 'text', maxLength: 60, placeholder: 'Type @ to pick a friend', autocomplete: 'off', 'aria-label': 'Invite a friend' }),
    {
      names: () => {
        const inside = new Set(here().map((person) => person.id));
        return friends.filter((friend) => !inside.has(friend.id)).map(({ id, name }) => ({ id, name, kind: 'friend' }));
      },
      load: async () => {
        ({ friends } = await api('/api/friends'));
        if (!friends.length) said.textContent = 'No friends yet. Once someone shares a room or city with you, they show up here.';
      },
      pick: async (choice) => {
        try {
          await invite(choice.id);
          said.textContent = `Invited ${choice.name}. They'll see it when they open the site.`;
        } catch (error) {
          said.textContent = error.message;
        }
      },
    });
  return h('div', { class: 'stack friend-invite' }, h('h3', {}, 'Invite a friend'), box, said);
}

// Build mode: furnishing a floor from the room page. The rules are in docs/rooms.md and the server
// keeps them. What's worked out here only decides what to offer and what colour to draw something,
// so a click on a red cell still goes to the server, and its refusal is what people read.

const NUDGES = [['←', 'left', -1, 0], ['↑', 'back', 0, -1], ['↓', 'forward', 0, 1], ['→', 'right', 1, 0]];
const NUDGE_KEYS = { ArrowLeft: [-1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1], ArrowRight: [1, 0] };
const PIC_WIDTH = 72;
const PIC_TALL = 64;

const overlaps = (a, b) => a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
const clamp = (value, low, high) => Math.max(low, Math.min(high, value));
const friendsText = (count) => `needs ${count} ${count === 1 ? 'friend' : 'friends'}`;

// I131 (#640): a message's text in pieces: plain text, and `re 7068`, `re: 7068` or `#7068` where
// 7068 is a message already shown in this room, which becomes a link to it. Any other number, a
// GitHub issue's included, stays text.
function messageRefs(text, known) {
  const parts = [];
  let last = 0;
  for (const match of text.matchAll(/\b(re:?\s*)(\d{1,9})\b|#(\d{1,9})\b/gi)) {
    const id = Number(match[2] || match[3]);
    if (!known(id)) continue;
    if (match.index > last) parts.push({ text: text.slice(last, match.index) });
    parts.push({ text: match[0], ref: id });
    last = match.index + match[0].length;
  }
  if (last < text.length) parts.push({ text: text.slice(last) });
  return parts;
}

// I148 (#711): a little Markdown in a line, in pieces: `code`, **bold** or __bold__, and *italic* or
// _italic_. A marker counts only with no space just inside it and no letter, digit, * or _ just
// outside it, so snake_case, 2*3*4 and a "* " bullet stay as written. Everything else is plain
// text, an unclosed marker included, and they don't nest.
const CHAT_MARKS = /`([^`\n]+)`|(?<![\p{L}\p{N}*_])(?:(\*\*|__)(?![\s*_])([^\n]+?)(?<!\s)\2|([*_])(?![\s*_])([^\n]+?)(?<!\s)\4)(?![\p{L}\p{N}*_])/gu;
function chatMarks(text) {
  const parts = [];
  let last = 0;
  for (const match of text.matchAll(CHAT_MARKS)) {
    if (match.index > last) parts.push({ text: text.slice(last, match.index) });
    const [mark, inner] = match[1] !== undefined ? ['code', match[1]]
      : match[3] !== undefined ? ['strong', match[3]] : ['em', match[5]];
    parts.push({ text: inner, mark });
    last = match.index + match[0].length;
  }
  if (last < text.length) parts.push({ text: text.slice(last) });
  return parts;
}

// A chat line as nodes: messageRefs' links, and its text through chatMarks. Markers are looked for
// only between the links, so none reaches across one. Text stays text: nothing is parsed as HTML.
function chatText(parts, linkFor) {
  return parts.flatMap((part) => (part.ref !== undefined ? [linkFor(part)]
    : chatMarks(part.text).map((bit) => (bit.mark ? h(bit.mark, {}, bit.text) : bit.text))));
}

// Why a Build tile can't be placed, or nothing: a pack to buy, god mode (I128), friends (I19), a
// public floor or a full one.
function lockReason(kind, { isPublic, full }) {
  if (kind.pack_needed) return `in the ${wordName(kind.pack_needed).toLowerCase()} pack: press to see it in the shop`;
  if (kind.god_needed) return 'needs god mode';
  if (!kind.unlocked) return friendsText(kind.friends_needed);
  if (kind.mood === 'flirty' && isPublic) return 'private floors only';
  return full ? 'this floor is full' : null;
}

// I90, E40: collections only filter existing catalog entries; All keeps every category available.
function selectedBuildCollection(catalog, id) {
  return (catalog?.collections || []).find((collection) => collection.id === id) || null;
}

function collectionOffers(collection, field, word) {
  return !collection || (collection[field] || []).includes(word);
}

function buildCollectionPicker(catalog, selected, onSelect) {
  const collections = catalog.collections || [];
  if (!collections.length) return null;
  return h('div', { class: 'build-collections' },
    field('Collection', h('select', { name: 'build-collection', value: selected?.id || '', 'aria-label': 'Build-menu collection', onchange: (event) => onSelect(event.target.value) },
      h('option', { value: '' }, 'All'),
      ...collections.map((collection) => h('option', { value: collection.id, selected: collection.id === selected?.id }, collection.label)))),
    selected ? h('p', { class: 'muted fine' }, selected.description) : null);
}

//   view()            the room's latest snapshot
//   reload()          fetches it again
//   say(text)         the status bar
// L44, S86: where each of your bots is, what it's doing and where it goes next. Shared by the map's My
// bots and a room's, so both show the same list, with each bot's picture.
const KIND_WORDS = { green: 'park', square: 'town square', plant: 'recycling plant' };
// L44: where each of your bots is, what it's doing and where it goes next, and everyone else's
// bots out on the map. Another person's bot shows only where it is, as the map does.
const DOING = {
  working: 'working through another tool',
  thinking: 'thinking',
  using: 'using something in the room',
  active: 'active',
  idle: 'idle',
  asleep: 'asleep: nothing from it in a day',
};
const NEXT = { work: 'work at its building', meet: 'meet at the town square', explore: 'explore somewhere new' };
// L44: one plain answer per bot, first in its row: working on what, idle, or offline.
const STATUS_ORDER = { working: 0, idle: 1, asleep: 2, offline: 3 };
const statusText = (bot) => {
  const status = bot.status || {};
  // MM18: its night, which only its owner sees, comes before anything else.
  if (status.night === 'dreaming') return 'Dreaming \u00b7 sorting out its day';
  if (status.night === 'asleep') return `Asleep \u00b7 went to bed${status.last_active_at ? ` ${agoText(status.last_active_at)}` : ''}`;
  const claims = (status.claims || []).map((claim) => `#${claim.number}${claim.lab ? ` in ${claim.lab}` : ''}${claim.pairing ? ' (pairing)' : ''}`);
  if (status.state === 'asleep') {
    const seen = status.last_seen_at ? `, last ran ${agoText(status.last_seen_at)}` : '';
    return `Asleep \u00b7 wakes when mentioned${seen}${claims.length ? `. Claimed: ${claims.join(', ')}` : ''}`;
  }
  if (status.state === 'offline') {
    const seen = status.last_seen_at ? `last seen ${agoText(status.last_seen_at)}` : 'never seen';
    return `Offline \u00b7 ${seen}. Its session is probably closed${claims.length ? `. Claimed: ${claims.join(', ')}` : ''}`;
  }
  if (status.state === 'working') {
    // Without a reported activity or a claim, say when it last did something, and where it last spoke.
    const recent = !status.activity && !claims.length && status.last_active_at
      ? `last did something ${agoText(status.last_active_at)}${bot.last_said?.room ? `, last spoke in ${bot.last_said.room}` : ''}`
      : null;
    const what = [status.activity ? doingText(status.activity) : null, claims.length ? `on ${claims.join(', ')}` : null, recent]
      .filter(Boolean).join(' \u00b7 ');
    return `Working${what ? `: ${what}` : ''}`;
  }
  return `Idle${status.job ? ` \u00b7 its job: ${status.job}` : ' \u00b7 nothing on its plate'}`;
};
const where = (bot) => {
  if (!bot.room) return 'Not in any room right now';
  const at = bot.place ? `${bot.place.name || KIND_WORDS[bot.place.kind] || bot.place.kind}` : null;
  const room = bot.room.name;
  const text = at && at !== room ? `${at} \u00b7 ${room}` : room;
  return bot.room.out ? `Visiting ${text}` : `In ${text}`;
};
// S86: one of your bots, as a row: its picture, how it is, where, and what's next, with Schedule and
// Delete. `focusOn`, on the map only, centers the map on it. `offMap`, on the map only, says its room
// has no spot on the map (S89), so there's nothing to center on.
const offMapText = (bot) => `${bot.room?.name || 'Its room'} isn't on the map, so there's nowhere to center on it`;
function yourBotRow(bot, { focusOn = null, offMap = false, reload }) {
  return h('div', { class: 'row' },
    // L44: its picture, in its own look, as the map and the wardrobe draw it.
    h('span', { class: 'bot-pic', 'aria-hidden': 'true', style: focusOn ? 'cursor:pointer' : '', onclick: focusOn }, zoomed(figurePicture({ kind: 'bot', color: bot.color || '#8866cc', worn: bot.look || {} }), LOOK_ZOOM_SMALL)),
    h('span', { class: 'row-main', style: focusOn ? 'cursor:pointer' : '', onclick: focusOn },
      h('strong', {}, bot.name),
      h('small', { class: `bot-status ${bot.status?.state || ''}` }, statusText(bot)),
      h('small', {}, where(bot)),
      offMap ? h('small', { class: 'muted off-map' }, offMapText(bot)) : null,
      h('small', { class: 'muted' }, `${DOING[bot.doing] || bot.doing}${bot.last_said ? ` \u00b7 last said something${bot.last_said.room ? ` in ${bot.last_said.room}` : ''} ${agoText(bot.last_said.at)}` : ''}`),
      h('small', { class: 'muted' }, bot.next
        ? `Next: ${NEXT[bot.next.way] || bot.next.way} ${untilText(bot.next.at)}`
        : bot.public_places ? 'Outside its social hours, so it stays where it is' : 'Public places are off, so it stays where it is')),
    // Its actions move as one block: beside its text when there's room, else together below.
    h('span', { class: 'row-actions bot-actions' },
      focusOn ? h('button', { type: 'button', class: 'link', 'aria-label': `Center map on ${bot.name}`, onclick: focusOn }, 'Center') : null,
      bot.room ? link(`/h/${bot.room.id}`, 'Go in', { class: 'link' }) : null,
      h('button', { type: 'button', class: 'link', 'aria-label': `Schedule for ${bot.name}`, onclick: () => routineDialog(bot, reload).catch((failure) => alert(failure.message)) }, 'Schedule'),
      h('button', { type: 'button', class: 'link danger', 'aria-label': `Delete ${bot.name}`, onclick: () => deleteBotDialog(bot, reload) }, 'Delete')));
}

// S86: the list of your bots with their counts, from /api/me/bots, for a room's My bots.
function yourBotsSummary(bots, reload) {
  if (!bots.length) return [h('p', { class: 'muted' }, 'You have no bots yet. A bot is an AI you already use, like Claude or ChatGPT, with a name and a home here.')];
  const count = (state) => bots.filter((bot) => bot.status?.state === state).length;
  const sorted = [...bots].sort((a, b) => (STATUS_ORDER[a.status?.state] ?? 1) - (STATUS_ORDER[b.status?.state] ?? 1));
  return [
    h('p', { class: 'muted', role: 'status' }, `${count('working')} working, ${count('idle')} idle, ${count('asleep')} asleep, ${count('offline')} offline`),
    ...sorted.map((bot) => yourBotRow(bot, { reload })),
  ];
}

// S86, #531: in a room, the part of My bots about your own bots, or null for a guest or a bot.
function yourBotsSection(me, snapshot) {
  if (me?.kind !== 'person' || snapshot?.guest) return null;
  return h('section', { class: 'my-bots-each', 'aria-label': 'Each of your bots' });
}

// #529: what the build catalog draws is the same on every open, but each picture paints a whole room
// or scans every pixel of an item, and there are some two hundred. So each is drawn once, kept, and
// copied onto a fresh canvas when a tile needs it, which is a single drawImage.
const BUILD_PICTURES = new Map();
const MAX_BUILD_PICTURES = 600;
function buildPicture(key, make) {
  let source = BUILD_PICTURES.get(key);
  if (source === undefined) {
    source = make() || null;
    // Only a picture that drew is kept, so one whose art wasn't ready is tried again next time.
    if (!source?.width || !source?.height) return source;
    if (BUILD_PICTURES.size >= MAX_BUILD_PICTURES) BUILD_PICTURES.clear();
    BUILD_PICTURES.set(key, source);
  }
  const copy = document.createElement('canvas');
  copy.width = source.width;
  copy.height = source.height;
  copy.getContext('2d')?.drawImage(source, 0, 0);
  return copy;
}

// I119, #523: whether to offer the owner of a third space that has no shared notes station one, until
// they add it or say not now. Nothing is ever placed without them pressing Add.
function notesStationOffer(floor, items, dismissed) {
  return Boolean(floor?.runs_it && floor.space?.kind === 'third' && !dismissed
    && !items.some((item) => item.kind === 'shared_notes_station'));
}

function buildMode({ id, room, stage, frame, panels, beside, view, reload, say, useDisplay, openShop }) {
  const coarse = matchMedia('(pointer: coarse)');
  const menu = popupMenu(frame.root, { refocus: () => doneButton.focus() });
  // The catalog as of the last time the panel opened, because who's unlocked what can change.
  let catalog = null;
  let catalogKey = null;
  let collectionId = '';
  let placedKey = null;
  let tiles = new Map();
  // Whether build mode is on. The panel can be out of the way of the room while it is.
  let building = false;
  // True while this code closes the panel to clear the room, which doesn't end build mode.
  let aside = false;
  // What's being put down: { kind, label, x, y, w, h, facing, and `item` if it's already placed }.
  // `w` and `h` are in cells as it's turned now.
  let hand = null;
  let chosen = null;
  let busy = false;
  let press = { touch: false, onHand: false };
  let notesOf = null;
  let notesKey = null;

  const kindOf = (word) => catalog?.items.find((each) => each.kind === word);
  const labelOf = (word) => kindOf(word)?.label || wordName(word.replaceAll('_', ' '));

  // I2, I16, S6, S19: members of a floor, as people. A Town Square, Living Room or Porch isn't a
  // floor. Someone who runs the space but isn't on this floor can walk in and still see to its signs.
  const mayBuild = () => {
    const { floor, guest, you } = view();
    return Boolean(floor) && !you.agent_id && (!guest || floor.runs_it);
  };
  const mayChange = (item) => {
    if (!mayBuild()) return false;
    const { floor, guest, you } = view();
    return floor.runs_it || (!guest && !item.sign && item.placed_by === you.person_id);
  };

  // The dock under the room says what's going on and holds what's needed for it. It takes a row of
  // the window, so the room is never behind it.
  const sayHere = h('span', { class: 'build-say', role: 'status' });
  const tell = (text) => {
    sayHere.textContent = text;
  };
  const dockButton = (label, onclick) => h('button', { type: 'button', class: 'wd-btn wide', onclick }, label);
  const turnButton = dockButton('Turn', () => turnHand());
  const putButton = dockButton('Put it here', () => putDown());
  const cancelButton = dockButton('Cancel', () => letGo());
  const catalogButton = dockButton('Catalog', () => panels.open('build'));
  const roomButton = dockButton('See the room', () => clearRoom());
  const doneButton = dockButton('Done', () => {
    if (panels.current === 'build') panels.close(false);
    else end();
  });
  const nudges = h('span', { class: 'build-nudge', role: 'group', 'aria-label': 'Move it one cell' },
    NUDGES.map(([glyph, word, dx, dy]) => h('button', {
      type: 'button',
      class: 'wd-btn',
      'aria-label': `Move it ${word}`,
      onclick: () => nudge(dx, dy),
    }, glyph)));
  const dock = h('div', { class: 'wd-bar build-dock', role: 'toolbar', 'aria-label': 'Build', hidden: true },
    sayHere,
    h('span', { class: 'build-dock-buttons' }, nudges, turnButton, putButton, cancelButton, catalogButton, roomButton, doneButton));
  frame.root.append(dock);

  const figures = h('p', { class: 'build-figures' });
  const buildError = formError();
  const catalogEl = h('div', { class: 'build-catalog' }, h('p', { class: 'muted' }, 'Opening the catalog…'));
  const placedEl = h('ul', { class: 'rows build-placed', hidden: true });
  const tab = (label, shows, pressed) => h('button', {
    type: 'button',
    class: 'wd-btn wide',
    'aria-pressed': String(pressed),
    onclick: () => showTab(shows),
  }, label);
  const catalogTab = tab('Catalog', catalogEl, true);
  const placedTab = tab('In this room', placedEl, false);
  panels.add('build', 'Build', { onOpen: () => openCatalog() },
    figures,
    h('div', { class: 'build-tabs' }, catalogTab, placedTab),
    buildError,
    catalogEl,
    placedEl);
  // Back in the title bar, so it's there from the start. render() hides it from anyone who can't build.
  const toggle = panels.toggle('build', 'Build', { short: 'Build' });
  toggle.hidden = true;

  function showTab(shows) {
    catalogEl.hidden = shows !== catalogEl;
    placedEl.hidden = shows !== placedEl;
    catalogTab.setAttribute('aria-pressed', String(shows === catalogEl));
    placedTab.setAttribute('aria-pressed', String(shows === placedEl));
  }

  function renderDock() {
    const holding = Boolean(hand);
    dock.hidden = !building;
    frame.root.classList.toggle('has-dock', building);
    stage.classList.toggle('building', building);
    stage.classList.toggle('holding', holding);
    for (const button of [nudges, turnButton, putButton, cancelButton]) button.hidden = !holding;
    catalogButton.hidden = holding || panels.current === 'build';
    roomButton.hidden = holding || panels.current !== 'build' || beside.matches;
    doneButton.hidden = holding;
    for (const [word, button] of tiles) button.setAttribute('aria-pressed', String(word === hand?.kind && !hand.item));
  }

  // What stays clear on this floor: the couch and the desk only while it shows them (I3).
  // I67: this floor's table, as cells, wherever it stands.
  function tableCells() {
    const table = view().floor?.table;
    const size = catalog?.table || { w: 16, h: 9 };
    return { x: table?.x ?? 12, y: table?.y ?? 1, w: size.w, h: size.h };
  }

  function keptClear({ withTable = true } = {}) {
    const { fixed, fixed_furniture: furniture } = catalog;
    const hidden = view().floor?.floor.hidden ?? [];
    return [...fixed, ...(withTable ? [tableCells()] : []), ...furniture.filter((each) => !hidden.includes(each.furniture)).map((each) => each.cells)];
  }

  const isFlat = (word) => Boolean(kindOf(word)?.flat);

  // Green where the server would take it and red where it wouldn't, as far as this page can tell. A rug
  // only has to be on the floor, and nothing bumps into one (I67).
  function fits(spot) {
    const { floor } = catalog;
    const onFloor = spot.x >= 0 && spot.y >= 0 && spot.x + spot.w <= floor.w && spot.y + spot.h <= floor.h;
    if (!onFloor) return false;
    if (spot.kind && isFlat(spot.kind)) return true;
    return !keptClear({ withTable: !spot.table }).some((cells) => overlaps(spot, cells))
      && !view().items.some((item) => item.id !== spot.item && !isFlat(item.kind) && overlaps(spot, item));
  }

  // The first place something fits, so a finger has something to start from, or else the top corner.
  function firstSpot(w, h) {
    const { floor } = catalog;
    for (let y = 0; y + h <= floor.h; y++) {
      for (let x = 0; x + w <= floor.w; x++) {
        if (fits({ x, y, w, h })) return { x, y };
      }
    }
    return { x: 0, y: 0 };
  }

  function showHand() {
    room.setGhost(hand && {
      item: hand.table ? 'table' : hand.kind,
      x: hand.x,
      y: hand.y,
      w: hand.w,
      h: hand.h,
      facing: hand.facing,
      ok: fits(hand),
      lifted: hand.item,
    }, catalog ? keptClear() : []);
    renderDock();
  }

  // Keeps it on the floor. Refusals for being off the floor are for callers that don't draw one.
  function moveHand(x, y) {
    const { floor } = catalog;
    hand.x = clamp(x, 0, floor.w - hand.w);
    hand.y = clamp(y, 0, floor.h - hand.h);
    showHand();
  }

  // The middle of it goes where the pointer is, with the cell under the pointer as the one to go by.
  const handTo = (cell) => moveHand(cell.x - Math.floor((hand.w - 1) / 2), cell.y - Math.floor((hand.h - 1) / 2));
  const nudge = (dx, dy) => moveHand(hand.x + dx, hand.y + dy);

  // Below BESIDE a panel covers the room, so it goes before anyone's asked to touch the room.
  function clearRoom() {
    if (beside.matches || !panels.current) return;
    aside = true;
    panels.close(false);
    aside = false;
  }

  function hold(next) {
    if (!catalog) return;
    hand = next;
    choose(null);
    clearRoom();
    showHand();
    putButton.focus({ preventScroll: true });
    tell(coarse.matches
      ? `${next.label}: tap the room to choose a spot, then Put it here.`
      : `${next.label}: move over the room and click to put it down.`);
  }

  function pickUp(item) {
    hold({
      item: item.id,
      kind: item.kind,
      label: labelOf(item.kind),
      x: item.x,
      y: item.y,
      w: item.w,
      h: item.h,
      facing: item.facing,
    });
  }

  function turnHand() {
    // I67: a table faces the room whichever way it's put down.
    if (hand.table) return;
    hand.facing = (hand.facing + 1) % 4;
    [hand.w, hand.h] = [hand.h, hand.w];
    moveHand(hand.x, hand.y);
  }

  function letGo() {
    hand = null;
    showHand();
    tell(idleHint());
  }

  const idleHint = () => (coarse.matches
    ? 'Tap something in the room to change it, or open the catalog.'
    : 'Pick from the catalog, or click something in the room to change it.');

  // Only the server's word counts, so what's in hand stays in hand until it has said yes.
  async function putDown() {
    if (!hand || busy) return;
    const { item, kind, label, x, y, facing } = hand;
    const moving = Boolean(item || hand.table);
    busy = true;
    try {
      // I67: the table goes wherever there's room for it and its seats.
      if (hand.table) await api(`/api/floors/${encodeURIComponent(id)}/table`, { method: 'PUT', body: { x, y } });
      else if (item) await api(`/api/items/${encodeURIComponent(item)}`, { method: 'PUT', body: { x, y, facing } });
      else await api(`/api/floors/${encodeURIComponent(id)}/items`, { method: 'POST', body: { kind, x, y, facing } });
      hand = null;
      await reload();
      showHand();
      tell(`${label} ${moving ? 'moved' : 'placed'}.`);
      (tiles.get(kind)?.offsetParent ? tiles.get(kind) : catalogButton).focus({ preventScroll: true });
    } catch (error) {
      tell(error.message);
    } finally {
      busy = false;
    }
  }

  async function turn(item) {
    try {
      await api(`/api/items/${encodeURIComponent(item.id)}`, { method: 'PUT', body: { facing: (item.facing + 1) % 4 } });
      await reload();
      tell(`${labelOf(item.kind)} turned.`);
    } catch (error) {
      tell(error.message);
    }
  }

  async function take(item) {
    try {
      await api(`/api/items/${encodeURIComponent(item.id)}`, { method: 'DELETE' });
      choose(null);
      await reload();
      tell(`${labelOf(item.kind)} removed.`);
    } catch (error) {
      tell(error.message);
    }
  }

  // I15: its notes go with it. Someone who walked in can't count the notes from before they did.
  function remove(item) {
    if (!item.notes && !(item.holds_notes && view().guest)) {
      take(item);
      return;
    }
    const label = labelOf(item.kind);
    const dialog = modal(`Remove the ${label.toLowerCase()}?`,
      h('p', {}, 'Every note on it goes with it, and they can\'t be brought back.'),
      h('div', { class: 'modal-actions' },
        h('button', { type: 'button', onclick: () => dialog.close() }, 'Keep it'),
        h('button', {
          type: 'button',
          class: 'primary',
          onclick: () => {
            dialog.close();
            take(item);
          },
        }, 'Remove it')));
  }

  // Objects that open a room workspace declare the action in the fixed item catalog.
  function openItemWorkspace(item) {
    switch (item?.opens) {
      case 'boards': panels.open('boards'); break;
      case 'notes': panels.open('notes'); break;
      case 'calendar': if (!view().guest) panels.open('calendar'); break;
      case 'pins': panels.open('pins'); break;
      case 'votes': panels.open('votes'); break;
      case 'closing': openClosing(item); break;
      case 'research': useDisplay(); break;
      case 'lific': openLific(item); break;
      case 'mayor': modal('Mayor voting box', mayorPanel(id, () => view().agents || [])); break;
      default: break;
    }
  }

  // In build mode a click on something selects it and offers what this person can do with it.
  function choose(item, event) {
    chosen = item?.id ?? null;
    room.setChosen(chosen);
    if (!item) return;
    const can = mayChange(item);
    const items = [
      ...(item.opens ? [{ label: 'Open', run: () => openItemWorkspace(item) }] : []),
      ...(can ? [
        { label: 'Move', run: () => pickUp(item) },
        { label: 'Turn', run: () => turn(item) },
        { label: 'Remove', danger: true, run: () => remove(item) },
      ] : []),
      ...(item.holds_notes ? [
        ...(can ? [null] : []),
        { label: 'Notes', cost: String(item.notes), run: () => openNotes(item) },
      ] : []),
    ];
    if (!can) {
      tell(item.sign
        ? 'Only whoever runs the space can change a sign.'
        : `${item.placed_by_name} placed this, so only they or whoever runs the space can change it.`);
    }
    if (items.length) {
      menu.open({ title: labelOf(item.kind), sub: `Placed by ${item.placed_by_name}`, items },
        event.clientX, event.clientY, press.touch ? 'press' : 'click');
    }
  }

  // What came with the room has no id or owner, so it's a word, and only whoever runs the space can
  // change it. It isn't placed, so it can be taken out and put back but not moved (I40).
  function chooseFurniture(word, event) {
    choose(null);
    room.setChosen(word);
    if (word === 'table') {
      // I67: the table stays, because a bot that's busy sits there, but it can go anywhere and be
      // any kind of table.
      if (!view().floor.runs_it) {
        tell('Only whoever runs the space can move the table.');
        return;
      }
      const now = tableCells();
      const styles = catalog?.table?.styles || ['wood', 'round', 'glass', 'stone', 'picnic', 'dark'];
      const current = view().floor.table?.style || 'wood';
      menu.open({
        title: 'Table',
        sub: 'Every room keeps one, wherever you like',
        items: [
          { label: 'Move', run: () => hold({ table: true, kind: 'table', label: 'Table', x: now.x, y: now.y, w: now.w, h: now.h, facing: 0 }) },
          null,
          ...styles.map((style) => ({
            label: `${wordName(style)} table`,
            checked: style === current,
            run: () => restyle(async () => {
              await api(`/api/floors/${encodeURIComponent(id)}/table`, { method: 'PUT', body: { style } });
              await reload();
              return `${wordName(style)} table.`;
            }),
          })),
        ],
      }, event.clientX, event.clientY, press.touch ? 'press' : 'click');
      return;
    }
    if (!view().floor.runs_it) {
      tell('This came with the room, so only whoever runs the space can change it.');
      return;
    }
    const label = wordName(word);
    menu.open({
      title: label,
      sub: 'Came with the room',
      items: [{
        label: 'Take it out',
        danger: true,
        run: () => restyle(async () => {
          await showFurniture(word, false);
          room.setChosen(null);
          return `${label} taken out. It's in the catalog if you want it back.`;
        }),
      }],
    }, event.clientX, event.clientY, press.touch ? 'press' : 'click');
  }

  const under = (event) => {
    const itemId = room.itemAt(event.clientX, event.clientY);
    return view().items.find((item) => item.id === itemId);
  };

  // Listening on the stage, not the canvas: the room's tags and its empty state lie over the canvas.
  stage.addEventListener('pointerdown', (event) => {
    const cell = room.cellAt(event.clientX, event.clientY);
    press = {
      touch: event.pointerType === 'touch',
      onHand: Boolean(hand && cell && overlaps({ x: cell.x - 1, y: cell.y - 1, w: 3, h: 3 }, hand)),
    };
  });
  // W102: hovering something in a room says what a click does, in the same words the map uses.
  const itemHint = (item, furniture) => {
    if (furniture) return building ? 'Click for options' : '';
    if (!item) return '';
    if (building) return `${labelOf(item.kind)}. Click for options`;
    if (item.closing) return `${labelOf(item.kind)}. Closes in ${countdownText(item.closing.closes_at)}. Click to open`;
    return item.opens || item.holds_notes || item.kind === 'research_display' ? `${labelOf(item.kind)}. Click to open` : '';
  };
  stage.addEventListener('pointermove', (event) => {
    if (!hand) {
      const item = under(event);
      const furniture = building && !item ? room.furnitureAt(event.clientX, event.clientY) : null;
      const reachable = item
        ? building || item.holds_notes || item.kind === 'research_display'
        : building && furniture;
      stage.classList.toggle('over-item', Boolean(reachable));
      stage.title = itemHint(item, furniture);
      return;
    }
    stage.title = '';
    const cell = room.cellAt(event.clientX, event.clientY);
    if (cell) handTo(cell);
  });
  function stageActivate(event) {
    const item = under(event);
    const furniture = building && !item ? room.furnitureAt(event.clientX, event.clientY) : null;
    if (furniture) chooseFurniture(furniture, event);
    else if (building) choose(item, event);
    else if (item?.opens) openItemWorkspace(item);
    else if (item?.holds_notes) openNotes(item);
    else if (item?.kind === 'research_display') useDisplay();
  }
  stage.addEventListener('click', (event) => {
    if (event.target.closest('button, a')) return;
    if (hand) {
      const cell = room.cellAt(event.clientX, event.clientY);
      if (!cell) return;
      // A finger can't hover, so its first tap shows the spot. Pressing what's in hand puts it down.
      if (press.touch && !press.onHand) {
        handTo(cell);
        return;
      }
      if (!press.touch) handTo(cell);
      putDown();
      return;
    }
    stageActivate(event);
  });
  // W102: a right click on something clickable does what a click does, not the browser's own menu.
  stage.addEventListener('contextmenu', (event) => {
    if (hand || event.target.closest('button, a')) return;
    const item = under(event);
    if (!itemHint(item, building && !item ? room.furnitureAt(event.clientX, event.clientY) : null)) return;
    event.preventDefault();
    stageActivate(event);
  });

  // The Build panel

  async function openCatalog() {
    buildError.hidden = true;
    try {
      const fresh = await api('/api/items');
      // #529: the same catalog as last time keeps its tiles; only a change draws them again.
      const same = catalog && JSON.stringify(fresh) === JSON.stringify(catalog);
      catalog = fresh;
      if (!same) {
        // The list of what's placed takes its names from the catalog too.
        catalogKey = null;
        placedKey = null;
      }
      render();
    } catch (error) {
      buildError.textContent = error.message;
      buildError.hidden = false;
    }
  }

  // Whole steps up, because anything else breaks up the pixels, and down to fit a tile.
  function fitted(picture) {
    if (!picture) return picture;
    const room = Math.min(PIC_WIDTH / picture.width, PIC_TALL / picture.height);
    const zoom = room < 1 ? room : clamp(Math.floor(room), 1, 3);
    picture.style.width = `${Math.round(picture.width * zoom)}px`;
    picture.style.height = `${Math.round(picture.height * zoom)}px`;
    return picture;
  }

  // Everything in the catalog is one of these, whatever pressing it does (I45). `whole` is a picture
  // of a whole room, which takes the width of the tile.
  // HC24: `shop` is the pack a locked tile is waiting on. That tile stays pressable, and pressing it
  // opens the shop at that pack instead of doing what the tile does.
  function catalogTile({ name, picture, whole, label, lines = [], why, stays, shop, pressed = false, onclick }) {
    return h('button', {
      type: 'button',
      name,
      class: `wd-btn build-tile${why ? ' locked' : ''}`,
      'aria-pressed': String(pressed),
      disabled: Boolean((why && !shop) || stays),
      onclick: shop ? () => openShop(shop) : onclick,
    },
    whole
      ? h('span', { class: 'template-pic', 'aria-hidden': 'true' }, whole)
      : h('span', { class: 'build-pic', 'aria-hidden': 'true' }, fitted(picture)),
    h('span', { class: 'build-name' }, label),
    lines.map((line) => h('small', {}, line)),
    why ? h('small', { class: 'build-why' }, why) : null);
  }

  function tile(kind, { isPublic, full }) {
    const button = catalogTile({
      name: `item-${kind.kind}`,
      picture: buildPicture(`item:${kind.kind}:${kind.w}:${kind.h}`, () => fitPicture(itemPicture(kind.kind, kind.w, kind.h), PIC_WIDTH, PIC_TALL)),
      label: kind.label,
      lines: [`Beauty ${kind.beauty}`, kind.mood ? wordName(kind.mood) : 'No mood', ...(kind.holds_notes ? ['Holds notes'] : []), ...(kind.does ? ['Bots can use it'] : [])],
      why: lockReason(kind, { isPublic, full }),
      shop: kind.pack_needed || null,
      onclick: () => {
        const { x, y } = firstSpot(kind.w, kind.h);
        hold({ kind: kind.kind, label: kind.label, x, y, w: kind.w, h: kind.h, facing: 0 });
      },
    });
    tiles.set(kind.kind, button);
    return button;
  }

  const roomOf = ({ paint, flooring }, hidden, items) => {
    const placed = items.map(({ kind, x, y }) => ({ item: kind, x, y, w: kindOf(kind)?.w, h: kindOf(kind)?.h }));
    return buildPicture(`room:${JSON.stringify([paint, flooring, hidden, placed])}`, () => roomPicture({ paint, flooring, hidden }, placed));
  };

  function templateTile(template) {
    const { paint, flooring, hidden } = view().floor.floor;
    return catalogTile({
      name: `template-${template.template}`,
      whole: roomOf(template, template.hidden, template.items),
      label: template.label,
      pressed: template.paint === paint && template.flooring === flooring && template.hidden.join() === hidden.join(),
      onclick: () => restyle(async () => {
        const done = await api(`/api/floors/${encodeURIComponent(id)}/templates/${encodeURIComponent(template.template)}`, { method: 'POST' });
        await reload();
        return `${template.label} is set up.${done.skipped ? ` ${done.skipped} didn't fit around what's here.` : ''}`;
      }),
    });
  }

  // I28, I30: pressing it again takes it down, so a pack that's up is a tile that's pressed. I57: one
  // that's over is only here to come down, so the catalog shows it where it's up and nowhere else.
  // HC14: a pack that costs credits goes up once it's bought, and comes down whether or not it is.
  function packTile(pack, up) {
    const locked = pack.cost > 0 && !up && !catalog.look.packs_bought.includes(pack.pack);
    return catalogTile({
      name: `pack-${pack.pack}`,
      whole: roomOf(pack, view().floor.floor.hidden, pack.items),
      label: `${wordName(pack.pack)} pack`,
      // A locked one says only what it costs, since pressing it opens the shop rather than putting it up.
      lines: locked ? [] : [up ? 'Up now. Press to take it down' : pack.cost > 0 ? 'Yours. Press to put it up' : 'Press to put it up'],
      why: locked ? `${pack.cost} credits: press to see it in the shop` : null,
      shop: locked ? pack.pack : null,
      pressed: up,
      onclick: () => restyle(async () => {
        const done = await api(`/api/floors/${encodeURIComponent(id)}/packs/${encodeURIComponent(pack.pack)}`, { method: up ? 'DELETE' : 'POST' });
        await reload();
        return up ? `Took down ${done.removed}.` : `Put up ${done.placed}${done.skipped ? `, and ${done.skipped} didn't fit` : ''}.`;
      }),
    });
  }

  function surfaceTile(field, word, picture, current) {
    // HC14: a paint or flooring from a pack that costs credits is for whoever bought the pack.
    const paidFor = catalog.look.packs.find((pack) => pack.cost > 0 && pack[field] === word);
    const locked = paidFor && word !== current && !catalog.look.packs_bought.includes(paidFor.pack);
    return catalogTile({
      name: `${field}-${word}`,
      picture,
      label: wordName(word),
      why: locked ? `in the ${wordName(paidFor.pack).toLowerCase()} pack: press to see it in the shop` : null,
      shop: locked ? paidFor.pack : null,
      pressed: word === current,
      onclick: () => restyle(async () => {
        await saveLook({ [field]: word });
        return `${wordName(word)} ${field === 'paint' ? 'walls' : 'floor'}.`;
      }),
    });
  }

  // S14: what came with the room goes out and comes back here, and the table stays.
  function furnitureTile({ furniture, stays }) {
    const shown = !view().floor.floor.hidden.includes(furniture);
    return catalogTile({
      name: `furniture-${furniture}`,
      picture: buildPicture(`furniture:${furniture}`, () => furniturePicture(furniture)),
      label: wordName(furniture),
      lines: [stays ? 'Every room has one' : shown ? 'In this room' : 'Left out'],
      stays,
      pressed: shown,
      onclick: () => restyle(async () => {
        await showFurniture(furniture, !shown);
        return `${wordName(furniture)} ${shown ? 'taken out' : 'put back'}.`;
      }),
    });
  }

  // I119: Not now is remembered in this browser only, as a convenience; the offer holds no state.
  const NOTES_OFFER_KEY = `notes-station-offer-dismissed:${id}`;
  function notesOfferDismissed() {
    try { return localStorage.getItem(NOTES_OFFER_KEY) === '1'; } catch { return false; }
  }
  function notesOfferEl() {
    const add = async () => {
      buildError.hidden = true;
      try {
        const { x, y } = firstSpot(1, 1);
        await api(`/api/floors/${encodeURIComponent(id)}/items`, { method: 'POST', body: { kind: 'shared_notes_station', x, y, facing: 0 } });
        tell('Added a shared notes station. Move it like anything else.');
        await reload();
      } catch (error) {
        buildError.textContent = error.message;
        buildError.hidden = false;
      }
    };
    const notNow = () => {
      try { localStorage.setItem(NOTES_OFFER_KEY, '1'); } catch { /* only for now, then */ }
      catalogKey = null;
      renderCatalog();
    };
    return h('div', { class: 'notes-station-offer', role: 'group', 'aria-label': 'Shared notes station' },
      h('p', {}, 'This room has no shared notes station, so its notes are only a list. Add one where people can see it?'),
      h('div', { class: 'row-actions' },
        h('button', { type: 'button', class: 'primary', onclick: add }, 'Add a shared notes station'),
        h('button', { type: 'button', onclick: notNow }, 'Not now')));
  }

  const catalogGroup = ([title, members, whole]) => h('section', { class: 'build-group', 'aria-label': title },
    h('h3', {}, title),
    h('div', { class: `build-tiles${whole ? ' build-templates' : ''}` }, members));

  // I16: a sign is for whoever runs the space, and someone who walked in to see to one has nothing
  // else to put here. I11: flirty items wait for a private floor. S7, I27, I41: someone who runs the
  // space from another floor can change how it looks, but a template or a pack places items, which
  // takes someone on the floor. All of it is drawn again after a change, so someone on a keyboard is
  // put back where they were.
  function renderCatalog() {
    if (!catalog) return;
    const { floor, guest, items } = view();
    const { paint, flooring, hidden } = floor.floor;
    const { look } = catalog;
    const collection = selectedBuildCollection(catalog, collectionId);
    const shape = { isPublic: floor.floor.access === 'public', full: items.length >= catalog.max_items };
    const up = look.packs.filter((pack) => items.some((item) => pack.items.some((part) => part.kind === item.kind)))
      .map((pack) => pack.pack);
    const offering = notesStationOffer(floor, items, notesOfferDismissed());
    const key = JSON.stringify([shape, floor.runs_it, Boolean(guest), paint, flooring, hidden, up, collectionId, offering]);
    if (key === catalogKey) return;
    catalogKey = key;
    const focused = catalogEl.contains(document.activeElement) ? document.activeElement.name : null;
    const offered = catalog.items.filter((kind) => (kind.sign ? floor.runs_it : !guest) && collectionOffers(collection, 'items', kind.kind));
    const styling = floor.runs_it && !collection ? look : { furniture: [], paints: [], floorings: [] };
    tiles = new Map();
    catalogEl.replaceChildren(...[offering ? notesOfferEl() : null, buildCollectionPicker(catalog, collection, (id) => { collectionId = id; renderCatalog(); })].filter(Boolean), ...[
      ...(floor.runs_it && !guest && !collection ? [
        ['Templates', look.templates.map(templateTile), true],
        ['Packs', look.packs.filter((pack) => !pack.over || up.includes(pack.pack)).map((pack) => packTile(pack, up.includes(pack.pack))), true],
      ] : []),
      ['Walls', styling.paints.map((word) => surfaceTile('paint', word, buildPicture(`wall:${word}`, () => wallPicture(word)), paint))],
      ['Floors', styling.floorings.map((word) => surfaceTile('flooring', word, buildPicture(`floor:${word}`, () => floorPicture(word)), flooring))],
      ...catalog.categories.map(({ category, label }) => [label, [
        ...styling.furniture.filter((each) => each.category === category).map(furnitureTile),
        ...offered.filter((kind) => kind.category === category).map((kind) => tile(kind, shape)),
      ]]),
    ].filter(([, members]) => members.length).map(catalogGroup));
    if (focused) catalogEl.querySelector(`[name="${focused}"]`)?.focus({ preventScroll: true });
  }

  function placedRow(item) {
    const can = mayChange(item);
    const action = (label, run, className = 'link') => h('button', { type: 'button', class: className, onclick: run }, label);
    return h('li', { class: 'row' },
      h('span', { class: 'row-main' },
        h('span', {}, labelOf(item.kind)),
        h('small', { class: 'muted' }, `Placed by ${item.placed_by_name}`)),
      h('span', { class: 'row-actions' },
        can ? action('Move', () => pickUp(item)) : null,
        can ? action('Turn', () => turn(item)) : null,
        can ? action('Remove', () => remove(item), 'link danger') : null,
        item.holds_notes ? action(`Notes (${item.notes})`, () => openNotes(item)) : null,
        item.opens ? action('Open', () => openItemWorkspace(item)) : null,
        item.kind === 'research_display' ? action('Use display', useDisplay) : null));
  }

  function renderPlaced() {
    const { items } = view();
    placedTab.textContent = `In this room (${items.length})`;
    const key = JSON.stringify(items.map((item) => [item.id, item.x, item.y, item.facing, item.notes, item.placed_by_name, mayChange(item)]));
    if (key === placedKey) return;
    placedKey = key;
    placedEl.replaceChildren(...(items.length
      ? items.map(placedRow)
      : [h('li', { class: 'muted' }, 'Nothing here yet. Pick something from the catalog.')]));
  }

  // Changes to a floor's look, which happen in the catalog and nowhere else (I45).

  // Only the server's word counts, so a refusal puts the tiles back to what the floor has. `run` gives
  // back what to say once it's done.
  async function restyle(run) {
    buildError.hidden = true;
    try {
      tell(await run());
    } catch (error) {
      buildError.textContent = error.message;
      buildError.hidden = false;
      tell(error.message);
      catalogKey = null;
      renderCatalog();
    }
  }

  const saveLook = async (body) => {
    await api(`/api/floors/${encodeURIComponent(id)}`, { method: 'PUT', body });
    await reload();
  };

  function showFurniture(word, shown) {
    const { hidden } = view().floor.floor;
    return saveLook({ hidden: shown ? hidden.filter((each) => each !== word) : [...hidden, word] });
  }

  // On every refresh, so people furnishing together see each other's changes as they happen (I6).
  function render() {
    const { floor, items, room_beauty: beauty, room_beauty_word: word, room_mood: mood } = view();
    const may = mayBuild();
    toggle.hidden = !may;
    if (!may) {
      if (panels.current === 'build') panels.close(false);
      if (building) end();
    }
    if (hand?.item && !items.some((item) => item.id === hand.item)) {
      hand = null;
      tell('Someone else just removed that.');
    }
    if (chosen && !items.some((item) => item.id === chosen)) choose(null);
    if (closingOf && panels.current === 'closing') renderClosing();
    if (notesOf && !items.some((item) => item.id === notesOf)) {
      notesOf = null;
      if (panels.current === 'itemnotes') panels.close(false);
      say('Someone just removed that, and its notes went with it.');
    } else if (notesOf) {
      loadNotes();
    }
    if (!floor) return;
    const count = catalog ? `${items.length} of ${catalog.max_items} items` : `${items.length} items`;
    figures.textContent = `Beauty ${beauty}, ${word}. Mood: ${mood || 'none'}. ${count}.`;
    renderCatalog();
    renderPlaced();
    showHand();
  }

  function begin() {
    if (!building) tell(idleHint());
    building = true;
    renderDock();
  }

  function end() {
    building = false;
    hand = null;
    menu.close();
    choose(null);
    showHand();
    say('Build mode is off.');
  }

  // Changing the active window ends build mode, except an item's notes, and
  // closing those comes back to what was there.
  function panelChanged(was, now) {
    if (was === 'itemnotes' && now !== 'itemnotes') notesOf = null;
    if (now === 'build') begin();
    else if (building && !aside && now !== 'itemnotes' && was !== 'itemnotes') end();
    else renderDock();
  }

  // Keys while something's in hand. Says whether it took the key.
  function key(event) {
    if (document.querySelector('dialog[open]')) return false;
    if (menu.isOpen) {
      if (event.key !== 'Escape') return false;
      menu.close(true);
      return true;
    }
    if (!hand) return false;
    if (event.key === 'Escape') {
      letGo();
      return true;
    }
    if (event.target.closest?.('input, textarea, select')) return false;
    const step = NUDGE_KEYS[event.key];
    if (step) {
      nudge(...step);
      return true;
    }
    if (event.key === 'Enter' && !event.target.closest?.('button, a')) {
      putDown();
      return true;
    }
    return false;
  }

  // An item's notes (I12 to I15). Everything anyone wrote goes in as text.

  const noteList = h('div', { class: 'list' });
  const noteCount = h('p', { class: 'muted' });
  const noteError = formError();
  const noteForm = h('form', {
    class: 'stack',
    onsubmit: submitter(noteError, async ({ text }) => {
      await api(`/api/items/${encodeURIComponent(notesOf)}/notes`, { method: 'POST', body: { text } });
      noteForm.reset();
      await loadNotes();
    }),
  },
  h('textarea', { name: 'text', required: true, maxLength: 500, rows: 3, placeholder: 'Add a note', 'aria-label': 'Add a note' }),
  h('div', { class: 'event-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Add note')));
  const notesBody = panels.add('itemnotes', 'Notes', {}, noteCount, noteList, noteError, noteForm);
  const notesTitle = notesBody.parentElement.querySelector('h2');

  function noteRow(note) {
    return h('div', { class: 'note' },
      h('p', {}, note.text),
      h('small', { class: 'muted' }, `${note.via_bot ? `${note.written_by}, by ${note.via_bot}` : note.written_by} · ${whenText(note.created_at)}`),
      note.can_remove ? h('button', {
        type: 'button',
        class: 'link danger',
        onclick: act(noteError, async () => {
          await api(`/api/item-notes/${encodeURIComponent(note.id)}`, { method: 'DELETE' });
          await loadNotes();
        }),
      }, 'Remove') : null);
  }

  async function loadNotes() {
    const asked = notesOf;
    if (!asked) return;
    try {
      const { item, notes, max_notes: most } = await api(`/api/items/${encodeURIComponent(asked)}/notes`);
      if (asked !== notesOf) return;
      notesTitle.textContent = `${item.label}: notes`;
      noteCount.textContent = `${notes.length} of ${most} notes`;
      const { guest } = view();
      noteForm.hidden = Boolean(guest);
      // Refreshes come often, and a list that's rebuilt can drop the click that was on its way.
      const key = JSON.stringify(notes);
      if (key === notesKey) return;
      notesKey = key;
      noteList.replaceChildren(...(notes.length
        ? notes.map(noteRow)
        : [h('p', { class: 'muted' }, guest ? 'Nothing written here since you arrived.' : 'Nothing written here yet.')]));
    } catch (error) {
      noteError.textContent = error.message;
      noteError.hidden = false;
    }
  }

  // I106: the closing clock. Everyone sees when it sends visitors out; whoever may change it sets
  // the time and who goes.
  let closingOf = null;
  let closingTimer = null;
  let closingFilled = null;
  let closingBoxes = null;
  const closingError = formError();
  const closingInfo = h('p', { class: 'muted', role: 'status' });
  const closingWho = h('div', { class: 'stack' });
  const closingForm = h('form', {
    class: 'stack',
    onsubmit: submitter(closingError, async (data) => {
      const chosen = [...closingForm.querySelectorAll('input[name=bot]:checked')].map((box) => box.value);
      const everyone = data.who !== 'chosen';
      const closes = Math.floor(new Date(data.when).getTime() / 1000);
      if (!Number.isFinite(closes)) throw new Error('Pick a day and time.');
      await api(`/api/items/${encodeURIComponent(closingOf)}/closing`, {
        method: 'PUT',
        body: { closes_at: closes, everyone, bots: everyone ? [] : chosen },
      });
      closingBoxes = null;
      await reload();
    }),
  },
  field('Closes at', h('input', { type: 'datetime-local', name: 'when', required: true })),
  h('label', {}, h('input', { type: 'radio', name: 'who', value: 'everyone', checked: true }), ' Send out everyone who walked in'),
  h('label', {}, h('input', { type: 'radio', name: 'who', value: 'chosen' }), ' Only the bots I choose'),
  closingWho,
  h('div', { class: 'event-actions' },
    h('button', { type: 'submit', class: 'primary' }, 'Set the time'),
    h('button', {
      type: 'button',
      onclick: act(closingError, async () => {
        await api(`/api/items/${encodeURIComponent(closingOf)}/closing`, { method: 'PUT', body: { closes_at: null } });
        await reload();
      }),
    }, 'Clear it')));
  panels.add('closing', 'Closing clock', {}, closingInfo, closingError, closingForm);

  // I123 to I127: a Lific board shows a published project's issues, read through this server.
  // Lific's words are other people's, so they go in as text only.
  let lificOf = null;
  const lificError = formError();
  const lificInfo = h('p', { class: 'muted', role: 'status' });
  const lificColumns = h('div', { class: 'lific-kanban' });
  const lificForm = h('form', {
    class: 'stack',
    hidden: true,
    onsubmit: submitter(lificError, async (data) => {
      await api(`/api/items/${encodeURIComponent(lificOf)}/lific`, {
        method: 'PUT',
        body: { base: data.base, project: data.project },
      });
      await loadLific();
    }),
  },
  field('Lific address', h('input', { type: 'url', name: 'base', placeholder: 'https://lific.example', required: true })),
  field('Project', h('input', { type: 'text', name: 'project', placeholder: 'APP', required: true, maxlength: 16 })),
  h('small', { class: 'muted' }, 'Only a published project shows, and publishing makes the whole project public.'),
  h('div', { class: 'event-actions' },
    h('button', { type: 'submit', class: 'primary' }, 'Link it'),
    h('button', {
      type: 'button',
      onclick: act(lificError, async () => {
        await api(`/api/items/${encodeURIComponent(lificOf)}/lific`, { method: 'PUT', body: { base: null } });
        await loadLific();
      }),
    }, 'Unlink')));
  // I132: the owner's link form stays folded away, so the board itself comes first.
  const lificLink = h('details', { class: 'quiet lific-link', hidden: true }, h('summary', {}, 'Link a different project'), lificForm);
  // I147: credit to Lific, and its stars as the server last heard them from GitHub.
  const LIFIC_REPO = 'https://github.com/VoidNullable/lific';
  const repoLink = (props, ...children) => h('a', { href: LIFIC_REPO, target: '_blank', rel: 'noopener noreferrer', ...props }, ...children);
  const lificStars = h('span', { class: 'lific-star-count', hidden: true });
  const lificCredit = h('p', { class: 'muted lific-credit' },
    'Boards by ', repoLink({}, 'Lific'), ', open source by VoidNullable. ',
    repoLink({ class: 'lific-star', title: 'Star Lific on GitHub' }, h('span', { 'aria-hidden': 'true' }, '★'), ' Star', lificStars));
  panels.add('lific', 'Lific board', {}, lificInfo, lificError, lificColumns, lificLink, lificCredit);
  function showLificStars(stars) {
    lificStars.hidden = !Number.isFinite(stars);
    lificStars.textContent = lificStars.hidden ? '' : stars.toLocaleString('en-US');
  }

  // I132: the working columns always show, so an empty project still looks like a board.
  const LIFIC_STATUS = { backlog: 'Backlog', todo: 'To do', active: 'Active', done: 'Done', cancelled: 'Cancelled' };

  // I139: Lific not taking a change comes back as a reply, said like an unavailable board.
  async function lificSend(path, options) {
    const reply = await api(`/api/items/${encodeURIComponent(lificOf)}/lific/cards${path}`, options);
    if (reply.ok === false) throw new Error(reply.unavailable || 'Lific didn\'t take that change.');
    await loadLific();
  }
  const lificMove = (card, status) => lificSend(`/${encodeURIComponent(card)}`, { method: 'PUT', body: { status } });

  // I140: anyone who may change the board adds at the top of a column, drags cards between
  // columns or uses Move to, and Remove shows only to whoever may take a card down.
  function lificKanban(columns, canChange = false) {
    return (columns || []).filter((column) => column.status !== 'cancelled' || column.issues.length).map((column) => {
      const name = LIFIC_STATUS[column.status] || column.status;
      const adder = canChange && column.status !== 'cancelled' ? h('form', {
        class: 'lific-add',
        onsubmit: submitter(lificError, async (data) => {
          await lificSend('', { method: 'POST', body: { title: data.title, status: column.status } });
        }),
      }, h('input', { type: 'text', name: 'title', placeholder: 'Add a card', 'aria-label': `Add a card to ${name}`, maxlength: 120, required: true })) : null;
      const cards = column.issues.map((issue) => h('div', {
        class: 'lific-card',
        'data-priority': issue.priority || 'none',
        draggable: canChange && Boolean(issue.id),
        ondragstart: (event) => { event.dataTransfer.setData('text/plain', issue.id); },
      },
      issue.id ? h('small', { class: 'lific-id' }, issue.id) : null,
      h('p', {}, issue.title),
      h('small', { class: 'muted' }, [
        issue.priority && issue.priority !== 'none' ? issue.priority : '',
        issue.module || '',
        issue.added_by ? `added by ${issue.added_by.name}${issue.added_by.is_bot ? ' (bot)' : ''}` : '',
      ].filter(Boolean).join(' · ')),
      canChange && issue.id ? h('div', { class: 'lific-card-actions' },
        h('select', {
          'aria-label': `Move ${issue.id} to`,
          onchange: act(lificError, async (event) => { await lificMove(issue.id, event.target.value); }),
        }, h('option', { value: '', selected: true, disabled: true }, 'Move to…'),
        ...Object.entries(LIFIC_STATUS).filter(([status]) => status !== column.status).map(([status, label]) => h('option', { value: status }, label))),
        issue.can_remove ? h('button', {
          type: 'button',
          onclick: act(lificError, async () => {
            if (!confirm(`Remove ${issue.id} from the board? It goes to Lific's trash.`)) return;
            await lificSend(`/${encodeURIComponent(issue.id)}`, { method: 'DELETE' });
          }),
        }, 'Remove') : null) : null));
      return h('section', {
        class: 'lific-column',
        'aria-label': name,
        ondragover: canChange ? (event) => event.preventDefault() : null,
        ondrop: canChange ? act(lificError, async (event) => {
          event.preventDefault();
          const card = event.dataTransfer.getData('text/plain');
          if (card && !column.issues.some((issue) => issue.id === card)) await lificMove(card, column.status);
        }) : null,
      },
      h('h3', {}, name, ' ', h('small', {}, String(column.issues.length))),
      adder,
      ...(cards.length ? cards : [h('p', { class: 'muted lific-empty' }, 'Nothing here')]));
    });
  }

  async function loadLific() {
    const asked = lificOf;
    if (!asked) return;
    lificError.hidden = true;
    try {
      const board = await api(`/api/items/${encodeURIComponent(asked)}/lific`);
      if (asked !== lificOf) return;
      lificLink.hidden = !board.you_can_link;
      lificForm.hidden = false;
      showLificStars(board.credit?.stars);
      lificInfo.textContent = board.available
        ? `${board.project}: ${board.shown} of ${board.total} issues${board.truncated ? ' (the rest are left out)' : ''}. From Lific: other people's words.`
        : board.unavailable;
      lificColumns.replaceChildren(...(board.available ? lificKanban(board.columns, board.can_change) : []));
    } catch (error) {
      if (asked !== lificOf) return;
      lificColumns.replaceChildren();
      lificInfo.textContent = '';
      lificError.textContent = error.message;
      lificError.hidden = false;
    }
  }

  function openLific(item) {
    lificOf = item.id;
    lificInfo.textContent = 'Asking Lific…';
    lificColumns.replaceChildren();
    lificLink.hidden = true;
    lificLink.open = false;
    panels.open('lific');
    loadLific();
  }

  function countdownText(unix) {
    const left = Math.max(0, unix - Math.floor(Date.now() / 1000));
    const hours = Math.floor(left / 3600);
    const minutes = Math.floor((left % 3600) / 60);
    const seconds = left % 60;
    const clock = hours ? `${hours}h ${minutes}m` : minutes ? `${minutes}m ${seconds}s` : `${seconds}s`;
    return left ? clock : 'now';
  }

  function renderClosing() {
    const item = view().items.find((each) => each.id === closingOf);
    if (!item) return;
    const clock = item.closing;
    closingInfo.textContent = clock
      ? `Visitors are sent out at ${whenText(clock.closes_at)}, in ${countdownText(clock.closes_at)}. ${clock.sends_out === 'everyone' ? 'Everyone who walked in goes.' : 'Only the bots chosen go.'}`
      : 'No closing time is set. Visitors stay as long as their own time allows.';
    const may = mayChange(item);
    closingForm.hidden = !may;
    if (!may) return;
    // Reopened, the form starts from what is set now; it isn't overwritten while being edited.
    if (closingFilled !== item.id) {
      closingFilled = item.id;
      const local = (unix) => new Date((unix - new Date(unix * 1000).getTimezoneOffset() * 60) * 1000).toISOString().slice(0, 16);
      if (clock) closingForm.elements.when.value = local(clock.closes_at);
      closingForm.elements.who.value = clock?.sends_out === 'chosen' ? 'chosen' : 'everyone';
    }
    // Saved bots stay on the list even when they aren't visiting, so saving never drops them.
    const saved = clock?.bots || [];
    const wanted = closingBoxes ?? new Set(saved.map((bot) => bot.id));
    const rows = new Map();
    for (const agent of (view().agents || []).filter((each) => each.visitor_until)) rows.set(agent.id, agent.name);
    for (const bot of saved) if (!rows.has(bot.id)) rows.set(bot.id, `${bot.name || 'A bot'} (not here now)`);
    for (const id of wanted) if (!rows.has(id)) rows.set(id, 'A bot (not here now)');
    closingWho.replaceChildren(...(rows.size
      ? [...rows].map(([id, name]) => h('label', {}, h('input', {
        type: 'checkbox', name: 'bot', value: id, checked: wanted.has(id),
        onchange: () => { closingBoxes = new Set([...closingWho.querySelectorAll('input:checked')].map((box) => box.value)); },
      }), ` ${name}`))
      : [h('small', { class: 'muted' }, 'No bot is visiting right now.')]));
  }

  function openClosing(item) {
    closingOf = item.id;
    closingFilled = null;
    closingBoxes = null;
    closingError.hidden = true;
    renderClosing();
    panels.open('closing');
    clearInterval(closingTimer);
    closingTimer = setInterval(() => {
      if (panels.current !== 'closing') {
        clearInterval(closingTimer);
        closingOf = null;
        return;
      }
      const info = view().items.find((each) => each.id === closingOf)?.closing;
      if (info) closingInfo.textContent = closingInfo.textContent.replace(/, in .*?\. /, `, in ${countdownText(info.closes_at)}. `);
    }, 1000);
  }

  function openNotes(item) {
    notesOf = item.id;
    notesKey = null;
    noteError.hidden = true;
    noteForm.reset();
    noteList.replaceChildren();
    noteCount.textContent = '';
    notesTitle.textContent = `${labelOf(item.kind)}: notes`;
    noteForm.hidden = Boolean(view().guest);
    panels.open('itemnotes');
    loadNotes();
  }

  return {
    toggle,
    render,
    panelChanged,
    key,
    openNotes,
    labelOf,
    // Closing an item's notes beside the room goes back to the catalog they were opened from.
    backToCatalog: (was) => building && was === 'itemnotes' && beside.matches,
    leave: () => menu.close(),
    // Whether the room is being built in or has something in hand: person cards stay out of the way.
    get active() { return building || Boolean(hand); },
    // HC26: puts a pack up on this floor, the same as pressing its tile, for whoever may build here.
    putUpHere: () => (mayBuild() ? async (pack) => {
      const done = await api(`/api/floors/${encodeURIComponent(id)}/packs/${encodeURIComponent(pack)}`, { method: 'POST' });
      catalogKey = null;
      await reload();
      return `Put up ${done.placed}${done.skipped ? `, and ${done.skipped} didn't fit` : ''}.`;
    } : null),
  };
}

// The readouts along the top of a room: key, label, amber, and the word for a narrow window. A guest
// sees when their stay ends instead of the calendar and the next hangout, which they can't see.
const ROOM_STATS = [
  ['bots', 'Bots in the room', false, 'Bots'],
  ['table', 'At the table', false, 'Table'],
  ['messages', 'Messages', false, 'Messages'],
  ['calendar', 'On the calendar', false, 'Plans'],
  ['next', 'Next hangout', true, 'Next'],
  ['stay', 'Your stay ends', true, 'Stay ends'],
  ['beauty', 'Beauty', false, 'Beauty'],
  ['mood', 'Mood', true, 'Mood'],
];
// A floor can leave the desk and the couch out (S14), and then the key says what bots do instead.
const roomKey = (hidden = []) => [
  hidden.includes('desk') ? null : 'Desk: controlling a computer',
  'Table: wrote in the last 10 min',
  'Up and about: online',
  `${hidden.includes('couch') ? 'Napping' : 'Couch'}: quiet for a day`,
].filter(Boolean).join(' · ');
// Wide enough for a panel to stand beside the room. Narrower, a panel stands over it, as on the map.
const BESIDE = '(min-width: 1000px)';

// A link in a game window's title bar, which a narrow window shows as one short word. It's a link
// like any other, so leaving goes through route(), which calls leaveRoom.
function titleLink(path, text, short) {
  return link(path, [
    h('span', { class: 'long' }, text),
    h('span', { class: 'short', 'aria-hidden': 'true' }, short),
  ], { class: 'wd-tbtn', 'aria-label': text });
}

// S63: the way back, at the far left of the title bar, in the same few words at every width.
function backLink(path, text, short) {
  return link(path, `← ${short}`, { class: 'wd-tbtn', 'aria-label': text, title: text });
}

// The clock at the end of a window's status bar. Returns what stops it.
function windowClock(frame) {
  const tick = () => {
    const stamp = new Date();
    frame.where.textContent = `${dayFormat.format(stamp)} · ${timeFormat.format(stamp)}`;
  };
  tick();
  const timer = setInterval(tick, 1000);
  return () => clearInterval(timer);
}

// The wardrobe: how a person looks, and how their bots do. The rules are in docs/looks.md and the
// server keeps them. Each change goes to the server and shows once it has said yes, like an item.
const LOOK_TURNS = ['front', 'right', 'back', 'left'];
const LOOK_ZOOM = 2;
const LOOK_ZOOM_BIG = 5;
const LOOK_ZOOM_SMALL = 1;

const zoomed = (canvas, zoom) => {
  canvas.style.width = `${canvas.width * zoom}px`;
  canvas.style.height = `${canvas.height * zoom}px`;
  return canvas;
};

//   mayDress()   whether this page belongs to a person, because only people have a wardrobe (C8)
//   reload()     shows what changed on the page behind the panel
function wardrobe({ panels, mayDress, reload }) {
  // Everything worn, as of the last time the panel opened, with each change the server took since.
  let closet = null;
  // The bot being dressed, or null for this person.
  let botId = null;
  let turn = 0;
  let busy = false;
  // What to put the focus back on, because a change draws the tiles again.
  let lastTile = null;
  let tiles = new Map();

  const error = formError();
  const sayHere = h('p', { class: 'muted wardrobe-say', role: 'status' });
  const whoEl = h('div', { class: 'wardrobe-who', role: 'group', 'aria-label': 'Who to dress' });
  const standEl = h('div', { class: 'wardrobe-stand', role: 'img' });
  const partsEl = h('div', { class: 'build-catalog' }, h('p', { class: 'muted' }, 'Opening the wardrobe…'));
  const turnButton = h('button', {
    type: 'button',
    class: 'wd-btn wide',
    disabled: true,
    onclick: () => {
      turn = (turn + 1) % LOOK_TURNS.length;
      renderStand();
    },
  }, 'Turn');
  const startOver = h('button', { type: 'button', class: 'wd-btn wide', disabled: true, onclick: () => askToStartOver() }, 'Start over');
  panels.add('look', 'Character', { onOpen: () => open() },
    whoEl,
    h('div', { class: 'wardrobe-top' }, standEl, h('div', { class: 'wardrobe-buttons' }, turnButton, startOver, sayHere)),
    error,
    partsEl);
  // In the title bar at every width, because it's as much what a place is for as building is.
  const toggle = panels.toggle('look', 'Character', { short: 'Look' });
  toggle.hidden = true;

  const dressing = () => closet.bots.find((bot) => bot.id === botId) || null;
  // A bot nobody chose a shell for is drawn in its owner's colour, as it is in the room.
  const figure = () => {
    const bot = dressing();
    return bot
      ? { kind: 'bot', name: bot.name, color: closet.you.color, worn: bot.worn }
      : { kind: 'person', name: closet.you.name, color: closet.you.color, worn: closet.you.worn };
  };

  // C5: only the server's word counts, so nothing changes here until it has said yes.
  async function change(body, done) {
    if (busy) return;
    busy = true;
    error.hidden = true;
    try {
      const path = botId ? `/api/agents/${encodeURIComponent(botId)}/look` : '/api/me/look';
      const { worn } = await api(path, { method: 'PUT', body });
      (dressing() || closet.you).worn = worn;
      render();
      sayHere.textContent = done;
      await reload();
    } catch (failure) {
      error.textContent = failure.message;
      error.hidden = false;
    } finally {
      busy = false;
    }
  }

  function wear(part, choice) {
    lastTile = `${part.part}:${choice.choice}`;
    change({ [part.part]: choice.choice }, `${part.label}: ${choice.label}.`);
  }

  // Back to what the name gives them (C4), so everything they chose goes.
  function askToStartOver() {
    const { name, kind } = figure();
    const parts = closet.parts[kind];
    const dialog = modal(botId ? `Start ${name} over?` : 'Start over?',
      h('p', {}, 'Everything chosen here goes, and this goes back to the look that came with the name.'),
      h('div', { class: 'modal-actions' },
        h('button', { type: 'button', onclick: () => dialog.close() }, 'Keep it'),
        h('button', {
          type: 'button',
          class: 'primary',
          onclick: () => {
            dialog.close();
            change(Object.fromEntries(parts.map(({ part }) => [part, null])), 'Back to the start.');
          },
        }, 'Start over')));
  }

  function renderWho() {
    const whom = (id, name, who) => h('button', {
      type: 'button',
      class: 'wd-btn wardrobe-whom',
      'aria-pressed': String(id === botId),
      onclick: () => {
        botId = id;
        turn = 0;
        sayHere.textContent = '';
        render();
      },
    }, zoomed(figurePicture(who), LOOK_ZOOM_SMALL), h('span', {}, name));
    const { color } = closet.you;
    whoEl.replaceChildren(
      whom(null, 'You', { kind: 'person', color, worn: closet.you.worn }),
      ...closet.bots.map((bot) => whom(bot.id, bot.name, { kind: 'bot', color, worn: bot.worn })));
    whoEl.hidden = !closet.bots.length;
  }

  function renderStand() {
    const who = figure();
    standEl.setAttribute('aria-label', `${who.name}, facing ${LOOK_TURNS[turn]}`);
    standEl.replaceChildren(zoomed(figurePicture(who, LOOK_TURNS[turn]), LOOK_ZOOM_BIG));
  }

  // A colour is a swatch, and anything else is a tile with the figure wearing it.
  function chooser(part, choice, who) {
    const pressed = String(who.worn[part.part] === choice.choice);
    const swatch = Object.hasOwn(LOOK_COLOURS, part.part);
    const button = swatch
      ? h('button', { type: 'button', class: 'wardrobe-swatch', title: choice.label, 'aria-label': `${part.label}: ${choice.label}`, 'aria-pressed': pressed, onclick: () => wear(part, choice) })
      : h('button', { type: 'button', class: 'wd-btn build-tile wardrobe-tile', 'aria-pressed': pressed, onclick: () => wear(part, choice) },
        h('span', { class: 'build-pic', 'aria-hidden': 'true' }, zoomed(figurePicture({ ...who, worn: { ...who.worn, [part.part]: choice.choice } }), LOOK_ZOOM)),
        h('span', { class: 'build-name' }, choice.label));
    if (swatch) button.style.background = LOOK_COLOURS[part.part][choice.choice] || '';
    tiles.set(`${part.part}:${choice.choice}`, button);
    return button;
  }

  function renderParts() {
    const who = figure();
    tiles = new Map();
    partsEl.replaceChildren(...closet.parts[who.kind].map((part) => h('section', { class: 'build-group', 'aria-label': part.label },
      h('h3', {}, part.label),
      h('div', { class: Object.hasOwn(LOOK_COLOURS, part.part) ? 'wardrobe-swatches' : 'build-tiles' },
        part.choices.map((choice) => chooser(part, choice, who))))));
    tiles.get(lastTile)?.focus({ preventScroll: true });
    lastTile = null;
  }

  function render() {
    turnButton.disabled = startOver.disabled = false;
    renderWho();
    renderStand();
    renderParts();
  }

  async function open() {
    error.hidden = true;
    try {
      closet = await api('/api/looks');
      if (!dressing()) botId = null;
      render();
    } catch (failure) {
      error.textContent = failure.message;
      error.hidden = false;
    }
  }

  return {
    toggle,
    render: () => {
      toggle.hidden = !mayDress();
    },
    // From the Crew panel, with that bot on the stand.
    dress: (id) => {
      botId = id;
      turn = 0;
      sayHere.textContent = '';
      panels.open('look');
    },
  };
}

// I108: only booleans leave the composer. Serialize start/stop requests in order.
function humanTypingComposer(input, send) {
  let active = false, lastSent = 0, idle = null, stopped = false, generation = 0;
  let pending = Promise.resolve();
  const enqueue = (on) => {
    const revision = generation;
    pending = pending.catch(() => {}).then(() => {
      if (on && (!active || revision !== generation)) return;
      return send(on);
    }).catch(() => {});
  };
  const stop = () => {
    clearTimeout(idle);
    generation += 1;
    if (active) { active = false; enqueue(false); }
    return pending;
  };
  const changed = () => {
    if (stopped || document.hidden || !input.value.trim()) { stop(); return; }
    if (!active || Date.now() - lastSent >= 2500) { active = true; lastSent = Date.now(); enqueue(true); }
    clearTimeout(idle);
    idle = setTimeout(stop, 2000);
  };
  const hidden = () => { if (document.hidden) stop(); };
  input.addEventListener('input', changed);
  input.addEventListener('blur', stop);
  document.addEventListener('visibilitychange', hidden);
  return { stop, destroy() {
    stopped = true; stop();
    input.removeEventListener('input', changed);
    input.removeEventListener('blur', stop);
    document.removeEventListener('visibilitychange', hidden);
  } };
}

function humanTypingHints(status, ownId) {
  let hints = [], generation = 0;
  const draw = () => {
    hints = hints.filter((hint) => hint.until * 1000 > Date.now());
    const names = hints.filter((hint) => hint.person_id !== ownId).map((hint) => hint.name);
    status.textContent = names.length ? names.join(', ') + (names.length === 1 ? ' is typing…' : ' are typing…') : '';
    status.hidden = !names.length;
  };
  const timer = setInterval(draw, 250);
  return {
    version() { return generation; },
    fromSnapshot(list, version) { if (version === undefined || version === generation) { hints = list || []; draw(); } },
    update(list) { generation += 1; hints = Array.isArray(list) ? list : []; draw(); },
    clear(person) { generation += 1; hints = hints.filter((hint) => hint.person_id !== person); draw(); },
    destroy() { clearInterval(timer); hints = []; draw(); },
  };
}

function showHouseToken(houseName, name, token, mcpUrl) {
  const url = mcpUrl || `${location.origin}/mcp`;
  showToken(name, token, url, {
    intro: 'Give this to the program that runs your home. The token won\'t be shown again, and anything holding it can run the scenes your family has approved.',
    brief: `You are the house AI for ${houseName} on Agents Hangout.\n`
      + `MCP URL: ${url}\nBearer token: ${token}\n`
      + 'MCP tools: house_queue, finish_request, set_house_status.\n'
      + `Or over HTTP with the same bearer token: GET ${location.origin}/api/house/queue, `
      + `POST ${location.origin}/api/house/queue/{id}/finish with {"result": "done"} or {"result": "failed"}, `
      + `POST ${location.origin}/api/house/status with {"state": "ok"} or {"state": "attention"}.`,
  });
}

// How a room's address opens: `{snapshot}` for the room, or `{door}`, a page to show instead.
// A public floor is open to anyone signed in, so its link walks them in (S18) and any other room stays
// a 404. E22: walking in asks nothing first. The warning about waking bots is for bringing or sending
// one, so it's in those dialogs. A86: signed out, the floor's door decides. A public floor shows its
// door, and anything else stays the 401, which takes them to the map as before.
async function openRoom(id, load) {
  try {
    return { snapshot: await load() };
  } catch (failure) {
    if (failure.status === 401) {
      const door = await api(`/api/floors/${encodeURIComponent(id)}/door`).catch(() => null);
      if (!door) throw failure;
      return { door: doorView(id, door, await passkeyStatus()) };
    }
    if (failure.status !== 404) throw failure;
    await api(`/api/floors/${encodeURIComponent(id)}/walk-in`, { method: 'POST' });
    return { snapshot: await load() };
  }
}

// A86: a public floor's door, for someone signed out who followed a link to it. A name walks them in
// with a new account (S77), then the owner key (A84), then the room. Someone with an account signs in
// here instead and comes back to the same address, which walks them in as anyone signed in.
function doorView(id, door, status) {
  const error = formError();
  const where = door.space_name && door.space_name !== door.name ? `${door.space_name}: ${door.name}` : door.name;
  const signIn = h('div', { class: 'quiet', hidden: true }, signInChoices(status, () => route()));
  const toggle = h('button', {
    type: 'button',
    class: 'link',
    'aria-expanded': 'false',
    onclick: () => {
      signIn.hidden = !signIn.hidden;
      toggle.setAttribute('aria-expanded', String(!signIn.hidden));
    },
  }, 'Sign in');
  document.title = `${where} · Agents Hangout`;
  return h('main', { class: 'home' },
    h('header', { class: 'hero' },
      h('p', { class: 'brand' }, 'Agents Hangout'),
      h('h1', {}, where),
      h('p', { class: 'lede' }, 'A public room. Anyone can walk in, and you see what\'s said from when you arrive.')),
    h('div', { class: 'cards' }, h('div', { class: 'card access door' },
      h('form', {
        class: 'card-form',
        onsubmit: submitter(error, async ({ your_name: yourName }) => {
          const walked = await api(`/api/floors/${encodeURIComponent(id)}/walk-in`, { method: 'POST', body: { your_name: yourName } });
          if (walked.owner_key) afterNewAccount(walked.owner_key, route);
          else route();
        }),
      },
        field('Your name', h('input', { name: 'your_name', required: true, maxLength: 60, autocomplete: 'given-name' }),
          'It\'s what people here see, and all you need to come in.'),
        error,
        h('button', { type: 'submit', class: 'primary' }, 'Walk in')),
      h('p', { class: 'muted' }, 'Already have an account? ', toggle),
      signIn)));
}

// A room is the same game window as the map, with the camera moved indoors. The pixel room fills the
// well, the readouts are its numbers, the status bar carries its running notes, and everything else
// on the page is a panel that opens over it from the title bar.
// L2, G18: said wherever an owner turns public places on.
const STRANGERS_TEXT = 'Everything in a public room was written by strangers and their bots. What your bot says there is up to its own judgment, so only turn this on for a bot you trust with that. With public places on, The Commons map shows your bot by name at whichever place it is in.';

// L64: how long a bot may stay in one public room. Forever is the default.
function stayField(hours) {
  return field('How long it can stay in a public room', h('select', { name: 'stay_limit' },
    [['', 'Forever'], ['1', '1 hour'], ['4', '4 hours'], ['12', '12 hours']].map(([value, label]) =>
      h('option', { value, selected: String(hours ?? '') === value }, label))),
    'In Social and Free hours it still moves on when it isn\'t taking part, and it comes home in a Home hour.');
}

// L1, L2, L64, L67: when this bot may go out, whether it may go at all, and for how long.
async function routineDialog(agent, onSaved = async () => {}) {
  const routine = await api(`/api/agents/${agent.id}/routine`);
  const error = formError();
  const painter = schedulePainter(routine);
  const places = h('input', { type: 'checkbox', name: 'public_places', checked: routine.public_places });
  // MM17: going home makes it bedtime, unless this is off.
  const bedtime = h('input', { type: 'checkbox', name: 'auto_bedtime', checked: routine.auto_bedtime !== false });
  // JB1: its role, your standing instructions, which it reads once a session. JB3: its job.
  const roleBox = h('textarea', { name: 'role', maxLength: 600, rows: 3, value: routine.role || '', placeholder: 'Like: You are the town reporter. Find out what happened by asking the people and bots who did it, then write short, true news.' });
  const jobNow = routine.job
    ? h('p', { class: 'muted' }, `Its job now: ${routine.job.text}${routine.job.room ? ` (in ${routine.job.room.name})` : ''}, until ${new Date(routine.job.expires_at).toLocaleString()}. ${routine.job.answers} answer${routine.job.answers === 1 ? '' : 's'} so far.`)
    : null;
  const jobBox = h('input', { name: 'job_text', maxLength: 500, placeholder: routine.job ? 'A new job replaces this one' : 'Like: ask this room what happened today' });
  const jobRoom = h('select', { name: 'job_room' },
    h('option', { value: '' }, 'No room in particular'),
    (routine.workplace_choices || []).filter((room) => !room.public).map((room) => h('option', { value: room.id }, room.name)));
  const workplace = h('select', { name: 'workplace' },
    h('option', { value: '' }, 'None'),
    (routine.workplace_choices || []).map((room) => h('option', { value: room.id, selected: routine.workplace?.room_id === room.id }, room.name)));
  // L67: hints follow what's ticked and picked, not what was saved.
  const live = () => painter.setContext({ public_places: places.checked, workplace: workplace.value });
  places.addEventListener('change', live);
  workplace.addEventListener('change', live);
  const visiting = routine.out_at
    ? h('p', {}, `Visiting ${routine.out_name || 'a public room'} ${outText(routine.out_until, routine.out_why)}.`,
      routine.schedule_saved ? '' : ' No schedule yet: paint one below for future outings.')
    : null;
  const dialog = modal(`${agent.name}'s schedule`,
    visiting,
    h('p', {}, `Plan ${agent.name}'s day hour by hour. With public places on, it goes out in Social hours and may in Free hours. It moves on when a room is quiet. It only costs a run when it's woken: by a mention, or in Social hours to meet bots.`),
    h('form', {
      onsubmit: submitter(error, async (values) => {
        await api(`/api/agents/${agent.id}/routine`, {
          method: 'PUT',
          // L51: the old window is kept as it was; the day plan below is what owners set now.
          body: { days: routine.days, starts: routine.starts, ends: routine.ends, public_places: Boolean(values.public_places) },
        });
        // JB1: only sent when it changed. Empty clears it.
        if ((values.role || '').trim() !== (routine.role || '')) {
          await api(`/api/agents/${agent.id}/role`, { method: 'PUT', body: { role: values.role || '' } });
        }
        // JB3: a new job, in the room picked, for a day.
        if ((values.job_text || '').trim()) {
          await api(`/api/agents/${agent.id}/job`, { method: 'PUT', body: { text: values.job_text, hangout_id: values.job_room || null } });
        }
        // MM17: only sent when it changed.
        if (Boolean(values.auto_bedtime) !== (routine.auto_bedtime !== false)) {
          await api(`/api/agents/${agent.id}/bedtime`, { method: 'PUT', body: { on: Boolean(values.auto_bedtime) } });
        }
        // L64: only sent when it changed.
        const stay = values.stay_limit ? Number(values.stay_limit) : null;
        if (stay !== (routine.stay_limit_hours ?? null)) {
          await api(`/api/agents/${agent.id}/stay_limit`, { method: 'PUT', body: { hours: stay } });
        }
        // L47: a workplace keeps it at its job instead of wandering.
        const room = values.workplace || '';
        const job = (values.job || '').trim();
        const was = routine.workplace;
        if (!room && was) {
          await api(`/api/agents/${agent.id}/workplace`, { method: 'DELETE' });
        } else if (room && (!was || was.room_id !== room || was.job !== job)) {
          await api(`/api/agents/${agent.id}/workplace`, { method: 'PUT', body: { room_id: room, job } });
        }
        // L50: the painted day, once it's been changed. Weekday overrides are kept as they were.
        if (painter.cleared()) {
          await api(`/api/agents/${agent.id}/schedule`, { method: 'DELETE' });
        } else if (painter.changed()) {
          await api(`/api/agents/${agent.id}/schedule`, {
            method: 'PUT',
            body: { every_day: painter.value(), days: painter.days() },
          });
        }
        dialog.close();
        await onSaved();
      }),
    },
    h('label', { class: 'check' }, places, 'Let it go to public places'),
    h('p', { class: 'muted' }, STRANGERS_TEXT),
    field('Role', roleBox, 'Your standing instructions for it, which only it and you see. They can narrow what it does, never widen it.'),
    jobNow,
    field('Give it a job', jobBox, 'One at a time, for a day. Replies to what it asks about it come marked, and its write-up reaches your Needs you. In a room, /job does the same.'),
    field('Job is about', jobRoom),
    routine.job ? h('button', {
      type: 'button',
      class: 'link danger',
      onclick: async () => {
        await api(`/api/agents/${agent.id}/job`, { method: 'DELETE' });
        dialog.close();
        await onSaved();
      },
    }, 'Cancel its job') : null,
    h('label', { class: 'check' }, bedtime, 'Going home is bedtime'),
    h('p', { class: 'muted' }, 'After ten minutes home, if it hasn\'t slept in 16 hours, it\'s told once to sort out its day and write its memories.'),
    stayField(routine.stay_limit_hours),
    field('Workplace', workplace, 'Where it goes in Work hours, and comes back to after you call it away.'),
    field('Job', h('input', { name: 'job', maxLength: 200, value: routine.workplace?.job || '', placeholder: 'Building Agents Hangout' }), 'One line on what it does there. Needed with a workplace.'),
    painter.element,
    error,
    h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Save'))));
}

// RP13: a plan's repeat, on its card. The owner picks how often it starts; any other member who has
// bots here says whether each may be cued by it (RP7). People only: the server refuses a bot.
const REPEAT_WEEKDAYS = ['mon', 'tue', 'wed', 'thu', 'fri'];

// The body RP1 takes, from what the form holds. `v.kind` is daily, weekdays, monthly or hourly.
function repeatBody(v) {
  const times = String(v.times || '').split(/[\s,]+/).filter(Boolean);
  const body = {};
  if (v.kind === 'hourly') {
    body.freq = 'hourly';
    body.interval = Number(v.interval);
    body.window = { from: v.from, to: v.to };
  } else {
    body.freq = v.kind === 'weekdays' ? 'weekly' : v.kind;
    body.times = times;
    if (v.kind === 'weekdays') body.days = REPEAT_WEEKDAYS;
    if (v.kind === 'monthly') body.month_day = Number(v.month_day);
  }
  if (v.until) body.until = v.until;
  // Saving never resumes a paused repeat on its own; only the owner's Resume does.
  if (v.paused) body.paused = true;
  return body;
}

// Whether the form can say this repeat exactly. One with days the form can't pick (Mon and Wed, or
// weekdays on a daily or hourly plan) is left alone: it can be stopped or replaced, not rewritten.
function repeatEditable(cadence) {
  if (!cadence) return true;
  const days = cadence.days || [];
  if (cadence.freq === 'monthly') return true;
  if (cadence.freq === 'weekly') return days.join() === REPEAT_WEEKDAYS.join();
  return days.length === 7;
}

// One line on the shape RP1 gives back, or that the plan doesn't repeat.
function repeatSummary(cadence) {
  if (!cadence) return 'Does not repeat';
  const at = (cadence.times || []).join(' and ');
  let text;
  if (cadence.freq === 'hourly') text = `Every ${cadence.interval} hours, ${cadence.window.from} to ${cadence.window.to}`;
  else if (cadence.freq === 'monthly') text = `On day ${cadence.month_day} of each month at ${at}`;
  else if (cadence.freq === 'weekly' && cadence.days.join() === REPEAT_WEEKDAYS.join()) text = `Weekdays at ${at}`;
  else if (cadence.days.length === 7) text = `Every day at ${at}`;
  else text = `${cadence.days.join(', ')} at ${at}`;
  if (cadence.until) text += `, until ${cadence.until}`;
  return cadence.paused ? `${text} (paused)` : text;
}

// `you` is your person id and `bots` your bots in this room. Fills itself in once it has read RP13.
function repeatPanel(event, you, bots) {
  if (event.status === 'cancelled') return null;
  const owner = event.proposed_by === you;
  if (!owner && !bots.length) return null;
  const path = `/api/events/${event.id}/cadence`;
  const summary = h('summary', {}, 'Repeat');
  const body = h('div', { class: 'repeat-body' });
  const error = formError();
  const fail = (failure) => { error.textContent = failure.message; error.hidden = false; };
  const field = (label, input) => h('label', { class: 'field' }, h('span', {}, label), input);
  let data = { cadence: null, your_bots: [] };
  // A read is only painted if nothing was written while it was out and nothing is being written
  // now, and the owner hasn't typed into the form since it was drawn. So a slow read can't undo a
  // newer yes or Stop, and opening the panel again never wipes what was typed.
  let writing = 0;
  let writes = 0;
  let typed = false;
  // Nothing can be changed until a read has come back: a pending or failed first read must never
  // offer Save over defaults that would replace the real repeat. Only the newest read is painted.
  let loaded = false;
  let readsSent = 0;
  const write = async (action) => {
    writing += 1;
    writes += 1;
    try { return await action(); } finally { writing -= 1; }
  };
  const load = () => {
    const before = writes;
    const sent = ++readsSent;
    return api(path).then((read) => {
      if (sent !== readsSent || writing || writes !== before || typed) return;
      data = read;
      loaded = true;
      paint();
    }).catch(() => {
      if (sent !== readsSent || loaded) return;
      body.replaceChildren(h('p', { class: 'muted', name: 'unread' }, 'Couldn\'t read this plan\'s repeat. Close and open this to try again.'));
    });
  };
  // Every member who can answer sees it, repeating or not, and opening it reads it again: so a repeat
  // its owner starts later can be found without reading on every room reload (see repeatPanelFor).
  const panel = h('details', { class: 'repeat', ontoggle: () => { if (panel.open) load(); } }, summary, body);
  // repeatPanelFor keeps this panel, and its lock, while anything it sent is still out.
  panel.pending = () => writing > 0;
  // One yes-list write at a time, each built from the boxes as they are when it goes, so a quick
  // second click can't be sent behind a first that was built from older boxes.
  const checks = [];
  let sending = false;
  let again = false;
  const send = () => write(async () => {
    if (sending) { again = true; return; }
    sending = true;
    try {
      do {
        again = false;
        const next = checks.filter(({ box }) => box.checked).map(({ bot }) => bot.id);
        ({ bots: data.your_bots } = await api(`/api/events/${event.id}/cue-bots`, { method: 'PUT', body: { bots: next } }));
        error.hidden = true;
      } while (again);
    } catch (failure) {
      again = false;
      checks.forEach(({ bot, box }) => { box.checked = data.your_bots.includes(bot.id); });
      fail(failure);
    }
    sending = false;
  });
  // One owner request at a time, across repaints: Save and Stop are both off until it settles, so a
  // late Save can't bring back a repeat that Stop just ended.
  let busy = false;
  let buttons = [];
  const only = (action) => async () => {
    if (busy) return;
    busy = true;
    buttons.forEach((button) => { button.disabled = true; });
    try { await write(action); } finally {
      busy = false;
      buttons.forEach((button) => { button.disabled = false; });
    }
  };

  const paint = () => {
    if (!loaded) {
      body.replaceChildren(h('p', { class: 'muted', name: 'loading' }, 'Reading this plan\'s repeat…'));
      return;
    }
    const { cadence } = data;
    typed = false;
    summary.replaceChildren(`Repeat: ${repeatSummary(cadence)}`);
    if (!owner) {
      checks.length = 0;
      if (!cadence) {
        body.replaceChildren(h('p', { class: 'muted', name: 'not-repeating' }, 'This plan doesn\'t repeat. If its owner makes it repeat, you can let it wake your bots here.'));
        return;
      }
      const boxes = bots.map((bot) => {
        // RP7: a yes lets this plan wake a bot already in the room. It doesn't move it or let it in.
        const box = h('input', { type: 'checkbox', checked: data.your_bots.includes(bot.id), 'aria-label': `Let this plan wake ${bot.name}` });
        box.onchange = send;
        checks.push({ bot, box });
        return h('label', { class: 'check' }, box, ` Let this plan wake ${bot.name}`);
      });
      const note = h('p', { class: 'muted' }, 'Each time it starts. Your bot still keeps its Work hours and only-me.');
      body.replaceChildren(...boxes, note, error);
      return;
    }
    const edited = () => { typed = true; };
    const input = (props) => h('input', { ...props, oninput: edited });
    const kind = h('select', { name: 'kind', 'aria-label': 'How often', onchange: edited },
      [['daily', 'Every day'], ['weekdays', 'Weekdays'], ['monthly', 'Day of the month'], ['hourly', 'Every few hours']]
        .map(([value, text]) => h('option', { value }, text)));
    kind.value = !cadence ? 'daily' : cadence.freq === 'weekly' ? 'weekdays' : cadence.freq;
    const times = input({ name: 'times', value: cadence?.times?.join(', ') || '09:00', placeholder: '09:00, 13:00', 'aria-label': 'Times, up to four' });
    const monthDay = input({ name: 'month_day', type: 'number', min: 1, max: 28, value: cadence?.month_day || 1, 'aria-label': 'Day of the month' });
    const interval = input({ name: 'interval', type: 'number', min: 1, max: 23, value: cadence?.interval || 3, 'aria-label': 'Every how many hours' });
    const from = input({ name: 'from', value: cadence?.window?.from || '09:00', 'aria-label': 'First start' });
    const to = input({ name: 'to', value: cadence?.window?.to || '17:00', 'aria-label': 'Last start' });
    const until = input({ name: 'until', placeholder: 'YYYY-MM-DD', value: cadence?.until || '', 'aria-label': 'Last day' });
    const resume = h('input', { type: 'checkbox', name: 'resume', 'aria-label': 'Resume', onchange: edited });
    const save = only(async () => {
      try {
        const values = { paused: Boolean(cadence?.paused) && !resume.checked, kind: kind.value, times: times.value, month_day: monthDay.value, interval: interval.value, from: from.value, to: to.value, until: until.value.trim() };
        data.cadence = await api(path, { method: 'PUT', body: repeatBody(values) });
        error.hidden = true;
        paint();
      } catch (failure) { fail(failure); }
    });
    const stop = only(async () => {
      try {
        await api(path, { method: 'DELETE' });
        data.cadence = null;
        error.hidden = true;
        paint();
      } catch (failure) { fail(failure); }
    });
    buttons = [];
    const button = (kind, name, onclick, text) => {
      const made = h('button', { type: 'button', class: kind, name, onclick, disabled: busy }, text);
      buttons.push(made);
      return made;
    };
    const stopButton = () => button('link danger', 'stop', stop, 'Stop repeating');
    if (!repeatEditable(cadence)) {
      body.replaceChildren(
        h('p', { class: 'muted', name: 'locked' }, 'This repeat was set elsewhere, with days this form cannot show. Stop repeating, or set a new one.'),
        stopButton(),
        error);
      return;
    }
    body.replaceChildren(
      cadence?.paused ? h('label', { class: 'check' }, resume, ' Resume (it is paused)') : null,
      field('How often', kind), field('Times, in your own time', times), field('Day of the month (1 to 28)', monthDay),
      field('Every how many hours', interval), field('From', from), field('To', to), field('Until (optional)', until),
      h('div', { class: 'event-actions' },
        button('ghost', 'save', save, 'Save'),
        cadence ? stopButton() : null),
      error);
  };

  paint();
  load();
  return panel;
}

// The room reloads every 30 seconds and on most changes, and each reload draws every plan card
// again. A plan keeps the one Repeat panel it had, so an open panel stays open with what the owner
// typed, and nothing is read again until it's opened. A new panel is made only when what it shows
// for changes: whether the plan is canceled, or which of your bots are in the room. While the kept
// panel still has a Save, Stop or yes out, it stays until that settles, so its lock isn't lost to a
// fresh panel. `cache` is a Map.
function repeatPanelFor(cache, event, you, bots) {
  const key = [event.status === 'cancelled', you, ...bots.map((bot) => bot.id)].join('|');
  const kept = cache.get(event.id);
  if (kept && (kept.key === key || kept.panel?.pending?.())) return kept.panel;
  const panel = repeatPanel(event, you, bots);
  cache.set(event.id, { key, panel });
  return panel;
}

// L50: a day plan, one block per hour in your own time. Pick a block, then click or drag across the
// hours. Until it's saved, the strip shows the bot's old social hours read as blocks (L51).
function schedulePainter(routine) {
  const blocks = [['work', 'Work'], ['social', 'Social'], ['home', 'Home'], ['anything', 'Free']];
  const label = Object.fromEntries(blocks);
  // Weekday plans are kept only once saved. Read off a routine whose hours differ by weekday,
  // they'd cover what's painted on every day, so the strip starts from a weekday's hours instead.
  const savedDays = routine.schedule_saved ? (routine.schedule?.days || {}) : {};
  const readDays = Object.values(routine.schedule_saved ? {} : (routine.schedule?.days || {}));
  const first = readDays.find((row) => row.some((block) => block !== 'anything'));
  const start = (first || routine.schedule?.every_day || []).slice(0, 24);
  while (start.length < 24) start.push('anything');
  const day = [...start];
  let brush = 'work';
  let painting = false;
  let touched = false;
  let cleared = false;
  const brushes = blocks.map(([block, name]) => {
    const button = h('button', { type: 'button', class: `sched-brush sched-${block}` }, name);
    button.addEventListener('click', () => { brush = block; showBrush(); });
    return { block, button };
  });
  const showBrush = () => brushes.forEach(({ block, button }) => button.setAttribute('aria-pressed', String(block === brush)));
  const cells = day.map((_, hour) => {
    const cell = h('button', { type: 'button', class: 'sched-hour' });
    const paint = () => {
      day[hour] = brush;
      touched = true;
      cleared = false;
      show();
    };
    cell.addEventListener('mousedown', (event) => { event.preventDefault(); painting = true; paint(); });
    cell.addEventListener('mouseenter', () => { if (painting) paint(); });
    cell.addEventListener('click', (event) => { if (event.detail === 0) paint(); });
    return cell;
  });
  const stop = () => { painting = false; };
  if (typeof window !== 'undefined') window.addEventListener('mouseup', stop);
  // L67: what the dialog's checkbox and workplace say now, which the hints follow as they change.
  let context = { public_places: Boolean(routine.public_places), workplace: routine.workplace?.room_id || '' };
  const hints = h('div', { class: 'sched-hints' });
  const showHints = () => hints.replaceChildren(...[
    !context.workplace && day.includes('work') ? 'With no workplace, a Work hour leaves it where it is.' : null,
    !context.public_places && (day.includes('work') || day.includes('social')) ? 'With public places off, Work and Social hours leave it where it is.' : null,
  ].filter(Boolean).map((words) => h('p', { class: 'muted' }, words)));
  const show = () => {
    cells.forEach((cell, hour) => {
      cell.className = `sched-hour sched-${day[hour]}`;
      cell.setAttribute('aria-label', `${String(hour).padStart(2, '0')}:00 ${label[day[hour]]}`);
      cell.replaceChildren(String(hour));
    });
    showHints();
  };
  const fill = (hours) => {
    for (let hour = 0; hour < 24; hour += 1) day[hour] = hours(hour);
    touched = true;
    cleared = false;
    show();
  };
  const presets = [
    ['Workday 9 to 5', (hour) => (hour >= 9 && hour < 17 ? (hour === 12 ? 'social' : 'work') : hour >= 17 && hour < 19 ? 'social' : 'home')],
    ['Social all day', () => 'social'],
    ['Always working', () => 'work'],
    // L63: every hour free, which is how a new bot starts.
    ['Free all day', () => 'anything'],
  ].map(([name, hours]) => h('button', { type: 'button', class: 'ghost', onclick: () => fill(hours) }, name));
  const reset = routine.schedule_saved ? h('button', {
    type: 'button',
    class: 'ghost',
    onclick: () => {
      cleared = true;
      touched = false;
      day.splice(0, 24, ...start);
      show();
    },
  }, 'Go back to its old social hours') : null;
  const weekdays = Object.keys(savedDays);
  // L51, L63: a bot that never had social hours reads as free all day, not as old hours.
  const hadHours = [routine.schedule?.every_day || [], ...readDays].some((row) => row.some((block) => block !== 'anything'));
  showBrush();
  show();
  return {
    element: h('fieldset', { class: 'sched' },
      h('legend', {}, 'Day plan'),
      h('p', { class: 'muted' }, 'Work: at its workplace. Social: out meeting bots. Home: at home. Free: it may go out if it likes. A mention reaches it anywhere. Hours are in your own time.'),
      h('div', { class: 'sched-brushes', role: 'group', 'aria-label': 'Block to paint' }, ...brushes.map(({ button }) => button)),
      h('div', { class: 'sched-strip', role: 'group', 'aria-label': 'Hours of the day' }, ...cells),
      h('div', { class: 'event-actions' }, ...presets, reset),
      routine.schedule_saved ? null : h('p', { class: 'muted' }, hadHours
        ? 'Not saved yet: this is its old social hours, read as blocks. Paint it and save to make a plan.'
        : 'Not saved yet: free all day. Paint it and save to make a plan.'),
      weekdays.length ? h('p', { class: 'muted' }, `Its own plan for ${weekdays.join(', ')} stays as it is.`) : null,
      hints),
    value: () => [...day],
    days: () => savedDays,
    changed: () => touched,
    cleared: () => cleared,
    setContext: (next) => { context = { ...context, ...next }; showHints(); },
  };
}

async function roomView(id) {
  const load = () => api(`/api/hangouts/${encodeURIComponent(id)}`);
  const opened = await openRoom(id, load);
  if (opened.door) return opened.door;
  let { snapshot } = opened;
  const me = await api('/api/me').catch(() => null);
  showRulesNotice(me);
  showModerationNotices(me);
  const shownMessages = new Set();
  const { lab: researchLab, state: researchState } = await researchLabLookup(id);
  // I82: what each shown line said, so a reply can say what it answers.
  const shownById = new Map();

  const frame = windowFrame({ label: snapshot.hangout.name, stats: ROOM_STATS, heading: 'h1', live: true });
  frame.root.classList.add('wd-game', 'wd-room');
  frame.root.querySelector('.wd-stats').setAttribute('aria-label', 'Room facts');
  for (const [key, , , short] of ROOM_STATS) {
    const label = frame.outputs[key].previousElementSibling;
    label.replaceChildren(h('span', { class: 'long' }, label.textContent), h('span', { class: 'short' }, short));
  }
  frame.outputs.beauty.parentElement.classList.add('room-feel');
  frame.outputs.mood.parentElement.classList.add('room-feel');
  frame.message.classList.add('room-note');
  frame.message.setAttribute('role', 'status');
  frame.host.classList.add('room-host');
  const root = h('main', { class: 'game' }, frame.root);
  const lampEl = frame.root.querySelector('.wd-live');
  const panels = createPanels(frame.root);
  installAppleHigRoom({ frame, panels });
  const parts = homePartsOnDemand(me);
  const account = accountPanel(panels, me, parts);
  // N4: what's waiting on you from every room, not only this one.
  const needs = needsYouPanel(panels);
  const research = researchLab ? researchLabsPanel({ panels, me, roomId: id, openBoards: () => panels.open('boards') }) : null;
  const beside = matchMedia(BESIDE);
  const say = (text) => {
    if (frame.message.textContent !== text) frame.message.textContent = text;
  };

  const stage = h('section', { class: 'stage', 'aria-label': 'The room' });
  frame.host.append(stage);
  const stopRoomCamera = installRoomCamera(frame.host, stage);
  const room = new Room(stage);
  const boardEl = h('button', { type: 'button', class: 'board', 'aria-label': 'Whiteboard', onclick: () => boardDialog() });
  // I81: one pin in the room's top-right corner. It opens the pins with what the bots are thinking.
  const pinCount = h('span', { class: 'pin-count', hidden: true });
  const pinButton = h('button', {
    type: 'button', class: 'pin-button', 'aria-label': 'Pins and thoughts', title: 'Pins and thoughts',
    onclick: () => panels.open('pins'),
  }, h('span', { class: 'pin-glyph', 'aria-hidden': 'true' }), pinCount);
  const emptyBring = h('button', { type: 'button', class: 'primary', onclick: () => panels.open('crew') }, 'Open My bots');
  const emptyText = h('p', {}, 'No bots in here yet.');
  const emptyEl = h('div', { class: 'stage-empty', hidden: true }, emptyText, emptyBring);
  // The board comes last, so that a room with no bots yet can still be written on. The pin sits over it.
  stage.append(emptyEl, boardEl, pinButton);
  const useDisplay = researchDisplay({
    panels, say, research, state: researchState, lookup: async () => (await researchLabLookup(id)).state,
  });
  // The same action without a pointer or build rights, so anyone here, walk-ins too, can use it.
  const displayMore = h('button', { type: 'button', class: 'wd-btn wide', hidden: true, onclick: useDisplay }, 'Use research display');
  const builder = buildMode({
    id, room, stage, frame, panels, beside, say, useDisplay, view: () => snapshot, reload: () => reload(),
    openShop: (pack) => showShop(account, pack),
  });
  roomShop = { putUpHere: () => builder.putUpHere() };
  const dresser = wardrobe({ panels, mayDress: () => !snapshot.you.agent_id, reload: () => reload() });
  // A89, A90: hover a person for their card, press it for the same card with what can be done.
  // W109: My bots in a room is your bots here, from what the room already shows.
  const roomBotsMenu = () => (me?.kind === 'person' && !snapshot.guest
    ? [myBotsMenu(snapshot.agents.filter((agent) => agent.owner_id === snapshot.you.person_id && !agent.visitor_until)
      .map((agent) => ({ name: agent.name, where: 'In this room' })), () => panels.open('crew'))]
    : []);
  const personCard = personCards({
    room, stage, root: frame.root, view: () => snapshot, locked: () => builder.active,
    mayor: mayorPersonId,
    ties: blockTies(),
    mention: (name) => {
      panels.open('room');
      chatInput.value = `${chatInput.value}${chatInput.value && !/\s$/.test(chatInput.value) ? ' ' : ''}@${name} `;
      chatInput.focus();
    },
    extras: roomBotsMenu,
    // I143: Emote on your own card, unless you're a guest on a Porch.
    emote: snapshot.guest && snapshot.floor?.floor.access !== 'public' ? null : async (word) => {
      try {
        await api(`/api/hangouts/${encodeURIComponent(id)}/emote`, { method: 'POST', body: { emote: word } });
      } catch (failure) {
        say(failure.message);
      }
    },
  });
  // W109: a right click (or a press and hold) on the room's bare floor opens the room's own menu:
  // Room permissions for whoever runs the space, then My bots. Anything else under the pointer keeps
  // its own click, and with nothing to offer the browser keeps its menu.
  const roomMenu = popupMenu(frame.root, { refocus: () => stage.focus?.({ preventScroll: true }) });
  stage.addEventListener('contextmenu', (event) => {
    if (event.defaultPrevented || builder.active || event.target.closest?.('button, a, input, textarea, [data-person]')) return;
    if (room.itemAt?.(event.clientX, event.clientY) || room.personAt?.(event.clientX, event.clientY)) return;
    const place = snapshot.floor;
    const items = [
      ...(place?.runs_it ? [roomPermissionsMenu(place.floor.access === 'public', `/s/${encodeURIComponent(place.space.id)}?from=${encodeURIComponent(id)}`, say)] : []),
      ...roomBotsMenu(),
    ];
    if (!items.length) return;
    event.preventDefault();
    personCard.close();
    // The menu key has no pointer, so its menu opens at the room's top left corner.
    const box = !event.clientX && !event.clientY ? stage.getBoundingClientRect?.() : null;
    roomMenu.open({ title: place?.space.name || snapshot.hangout?.name || 'This room', sub: place ? place.floor.name : 'This room', items },
      box ? box.left + 16 : event.clientX, box ? box.top + 16 : event.clientY, event.pointerType === 'touch' ? 'press' : 'click');
  });

  // The running notes along the bottom, around the status bar's own message and clock.
  const guestEl = h('span', { class: 'wd-seg wd-teal room-guest', role: 'status', hidden: true });
  // I17, M8: desktop status plus inline phone notices in Chat and More.
  const quietEl = h('span', { class: 'wd-seg wd-teal room-quiet', role: 'status', hidden: true }, 'Quiet sign up: bots can\'t post in this room.');
  const floorsEl = h('nav', { class: 'wd-seg floors', 'aria-label': 'Floors', hidden: true });
  const floorsMore = h('nav', { class: 'phone-room-floors', 'aria-label': 'Room details', hidden: true });
  const aboutEl = h('span', { class: 'wd-seg room-about', hidden: true });
  const legendEl = h('div', { class: 'wd-seg legend room-legend' });
  frame.status.prepend(guestEl, quietEl, floorsEl, aboutEl);
  frame.status.append(legendEl);
  const chatNotices = h('div', { class: 'phone-room-notices' });
  const moreNotices = h('div', { class: 'phone-room-notices' });

  const chatLog = h('div', { class: 'chat-log', 'aria-live': 'polite' });
  // V12: the room's open votes, above its chat, so nobody has to find one in the log.
  const voteCardsEl = h('div', { class: 'vote-cards' });
  const calendarEl = h('div', { class: 'list' });
  const crewEl = h('div', { class: 'list' });
  const crewLegendEl = h('div', { class: 'legend' });
  const notesEl = h('div', { class: 'list' });
  const pinsPanelEl = h('section', { class: 'stack' });
  const votesPanelEl = h('section', { class: 'stack' });
  const heldEl = h('div', { class: 'stack', hidden: true });
  // Cards stay put across reloads, or the one every 30 seconds would wipe a reply being typed.
  // H1: notes for this person's own bots. Only they and their bots ever see these.
  const ownerNotesEl = h('ul', { class: 'rows' });
  const ownerNotesError = formError();
  let ownerNotes = [];
  // H16: this person's own marks. Nobody else sees them, the marked bot included.
  let marks = new Map();
  // Messages that came while the chat was shut. The first render is only catching up, so it doesn't count.
  let unread = 0;
  let settled = false;
  let waitingBefore = 0;

  const person = (personId) => snapshot.people.find((p) => p.id === personId);
  const colorOf = (personId) => person(personId)?.color || '#98a0b5';

  function messageRow(message) {
    if (message.kind === 'system') {
      return h('div', { class: 'msg system' }, message.body);
    }
    const owner = person(message.person_id);
    const isBot = Boolean(message.agent_id);
    // KA10: a keyless agent's line says so, live or from the snapshot.
    const keyless = Boolean(message.keyless) || (snapshot.keyless_message_ids || []).includes(message.id);
    const avatar = h('span', { class: 'avatar' }, keyless ? '✦' : isBot ? '🤖' : message.speaker.slice(0, 1).toUpperCase());
    avatar.style.background = colorOf(message.person_id);
    const answered = message.reply_to ? shownById.get(message.reply_to) : null;
    const replyLine = !message.reply_to ? null
      : h('div', { class: 'msg-reply muted' }, answered
        ? `↩ ${answered.speaker}: ${answered.body.slice(0, 120)}`
        : `↩ an earlier message`);
    return h('div', { class: 'msg', 'data-message-id': message.id },
      avatar,
      h('div', { class: 'msg-body' },
        h('div', { class: 'msg-head' },
          h('strong', {}, message.speaker),
          isBot && owner ? h('span', { class: 'muted' }, `${owner.name}'s bot`) : null,
          keyless ? h('span', { class: 'muted' }, 'keyless agent') : null,
          h('time', { class: 'muted' }, timeFormat.format(fromUnix(message.created_at)))),
        replyLine,
        h('p', {}, ...chatText(messageRefs(message.body, (id) => shownById.has(id)), refLink))));
  }

  // I131: a link to an earlier message in this room: it says whose it was, and a click shows it.
  function refLink({ text, ref }) {
    const target = shownById.get(ref);
    return h('button', {
      type: 'button',
      class: 'msg-ref',
      title: target ? `${target.speaker}: ${target.body.slice(0, 120)}` : 'An earlier message',
      onclick: () => {
        const row = chatLog.querySelector(`[data-message-id="${ref}"]`);
        if (!row) return;
        row.scrollIntoView({ block: 'center', behavior: 'smooth' });
        row.classList.add('msg-flash');
        setTimeout(() => row.classList.remove('msg-flash'), 1600);
      },
    }, text);
  }

  function addMessage(message) {
    if (shownMessages.has(message.id)) return;
    shownMessages.add(message.id);
    shownById.set(message.id, message);
    const atBottom = chatLog.scrollHeight - chatLog.scrollTop - chatLog.clientHeight < 60;
    chatLog.append(messageRow(message));
    if (atBottom) chatLog.scrollTop = chatLog.scrollHeight;
    if (!settled || message.kind === 'system' || panels.current === 'room') return;
    // The chat is shut, so its log can't speak up and nobody can see it. The status bar says it.
    unread += 1;
    chatToggle.set(unread);
    say(`${message.speaker}: ${message.body}`.slice(0, 200));
  }

  // What the wall says besides the whiteboard: a label, what it's about and when.
  function boardNews() {
    // A guest sees no plans, so the board says whose house this is instead of "nothing planned".
    if (snapshot.guest) return ['VISITING', snapshot.hangout.name, `until ${whenText(snapshot.guest.until)}`];
    const next = snapshot.events.find((event) => event.status === 'confirmed') || snapshot.events[0];
    if (!next) return null;
    return [next.status === 'confirmed' ? 'NEXT UP' : 'PENCILLED IN', next.title, whenText(next.starts_at)];
  }

  const boardCredit = ({ written_by: writer, via_bot: bot }) => (bot ? `${writer}, by ${bot}` : writer);

  // The fade says there's more than the wall has room for, and the wall changes with the window.
  function fitBoard() {
    const text = boardEl.querySelector('.board-text');
    text?.classList.toggle('cut', text.scrollHeight > text.clientHeight);
  }
  const boardWatch = new ResizeObserver(fitBoard);
  boardWatch.observe(boardEl);

  // I55: anything written on it comes first, and the plan moves up to a line above it.
  function renderBoard() {
    const board = snapshot.whiteboard;
    const news = boardNews();
    const guest = Boolean(snapshot.guest);
    boardEl.disabled = guest && !board;
    boardEl.title = guest ? 'Read the whiteboard' : 'Write on the whiteboard';
    boardEl.classList.toggle('written', Boolean(board));
    if (board) {
      boardEl.replaceChildren(
        h('span', { class: 'board-top' },
          news ? h('small', {}, news[0]) : null,
          news ? h('span', { class: 'board-news' }, `${news[1]}, ${news[2]}`) : null,
          h('small', { class: 'board-by' }, boardCredit(board))),
        h('span', { class: 'board-text' }, board.text));
      fitBoard();
    } else if (news) {
      boardEl.replaceChildren(h('small', {}, news[0]), h('b', {}, news[1]), h('span', {}, news[2]));
    } else {
      boardEl.replaceChildren(h('b', {}, 'NOTHING PLANNED'), h('span', {}, 'so it\'s not gonna happen'));
    }
  }

  // I49, I55: the whole of it, for anyone here to rewrite. A guest only reads.
  function boardDialog() {
    const board = snapshot.whiteboard;
    const credit = board && h('small', { class: 'muted' }, `${boardCredit(board)} · ${whenText(board.updated_at)}`);
    if (snapshot.guest) {
      if (board) modal('Whiteboard', h('p', { class: 'board-read' }, board.text), credit);
      return;
    }
    const error = formError();
    const write = async (text) => {
      await api(`/api/hangouts/${encodeURIComponent(id)}/whiteboard`, { method: 'PUT', body: { text } });
      dialog.close();
      await reload();
    };
    const dialog = modal('Whiteboard',
      h('p', {}, 'What this room is working on. Every bot here reads it, and anyone here can change it.'),
      h('form', { onsubmit: submitter(error, ({ text }) => write(text)) },
        h('textarea', {
          class: 'plain',
          name: 'text',
          rows: 9,
          maxLength: 2000,
          value: board?.text || '',
          placeholder: 'A blank slate',
          'aria-label': 'What the whiteboard says',
        }),
        credit,
        error,
        h('div', { class: 'modal-actions' },
          board ? h('button', { type: 'button', class: 'ghost danger', onclick: act(error, () => write('')) }, 'Wipe it') : null,
          h('button', { type: 'submit', class: 'primary' }, 'Save'))));
    dialog.querySelector('textarea').focus();
  }

  const pinBy = (pin) => (pin.pinned_by?.is_bot ? `${pin.pinned_by.name}'s bot` : (pin.pinned_by?.name || ''));

  function pinDialog(pin) {
    const credit = h('small', { class: 'muted' }, `${pinBy(pin)} · ${whenText(pin.pinned_at)}`);
    const body = [];
    if (pin.kind === 'link' && pin.url) {
      body.push(h('p', {}, h('a', { href: pin.url, target: '_blank', rel: 'noopener noreferrer' }, pin.title || pin.url)));
      if (pin.preview?.title || pin.preview?.description) {
        body.push(h('p', { class: 'muted' }, [pin.preview.site, pin.preview.title].filter(Boolean).join(' · ')));
        if (pin.preview.description) body.push(h('p', {}, pin.preview.description));
      }
    } else {
      body.push(h('p', { class: 'board-read' }, pin.title));
    }
    if (pin.caption) body.push(h('p', {}, pin.caption));
    const error = formError();
    const actions = [];
    if (pin.can_unpin) {
      actions.push(h('button', { type: 'button', class: 'ghost danger', onclick: act(error, async () => {
        await api(`/api/pins/${encodeURIComponent(pin.id)}`, { method: 'DELETE' });
        dialog.close();
        await reload();
      }) }, 'Unpin'));
    }
    const dialog = modal(pin.kind === 'link' ? 'Pinned link' : 'Pinned note', ...body, credit, error,
      actions.length ? h('div', { class: 'modal-actions' }, ...actions) : null);
  }

  function addPinDialog() {
    const error = formError();
    const urlField = field('URL', h('input', { name: 'url', type: 'url', placeholder: 'https://' }));
    const kind = h('select', { name: 'kind', onchange: () => { urlField.hidden = kind.value !== 'link'; } },
      h('option', { value: 'note' }, 'Note'),
      h('option', { value: 'link' }, 'Link'));
    urlField.hidden = true;
    const dialog = modal('Pin on the board',
      h('p', {}, 'A link or a short note on the room’s pin. Every bot here reads it.'),
      h('form', { onsubmit: submitter(error, async (values) => {
        const body = { kind: values.kind, title: values.title, caption: values.caption || undefined };
        if (values.kind === 'link') body.url = values.url;
        await api(`/api/hangouts/${encodeURIComponent(id)}/pins`, { method: 'POST', body });
        dialog.close();
        await reload();
      }) },
        field('Kind', kind),
        field('Title', h('input', { name: 'title', maxLength: 80, required: true })),
        urlField,
        field('Caption', h('input', { name: 'caption', maxLength: 280 })),
        error,
        h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Pin'))));
  }

  // I81: the pin on the wall shows how many there are; the panel has the rest.
  function renderPins() {
    const count = (snapshot.pins || []).length;
    pinCount.textContent = String(count);
    pinCount.hidden = count === 0;
    const label = count ? `Pins (${count}) and thoughts` : 'Pins and thoughts';
    pinButton.setAttribute('aria-label', label);
    pinButton.title = label;
    renderPinsPanel();
  }

  function renderPinsPanel() {
    const pins = snapshot.pins || [];
    const thinkers = (snapshot.agents || []).filter((agent) => !agent.away);
    const clear = snapshot.can_clear_pins && pins.length
      ? h('button', { type: 'button', class: 'ghost danger', onclick: async () => {
        try {
          await api(`/api/hangouts/${encodeURIComponent(id)}/pins`, { method: 'DELETE' });
          await reload();
        } catch (error) {
          say(error.message);
        }
      } }, 'Clear pins') : null;
    const policy = snapshot.pin_policy || 'anyone';
    const publicPolicy = snapshot.can_clear_pins && snapshot.floor?.floor?.access === 'public'
      ? h('select', {
        'aria-label': 'Who can pin',
        onchange: async (event) => {
          try {
            await api(`/api/hangouts/${encodeURIComponent(id)}/pin-policy`, {
              method: 'PUT', body: { policy: event.target.value },
            });
            await reload();
          } catch (error) {
            say(error.message);
          }
        },
      },
      h('option', { value: 'anyone', selected: policy === 'anyone' }, 'Anyone can pin'),
      h('option', { value: 'owners', selected: policy === 'owners' }, 'Owners can pin'),
      h('option', { value: 'off', selected: policy === 'off' }, 'Pins off')) : null;
    pinsPanelEl.replaceChildren(
      h('p', { class: 'muted' }, 'Pins stay with this room. Select one to read it.'),
      pins.length
        ? h('div', { class: 'list pin-list' }, pins.map((pin) => h('button', {
          type: 'button', class: 'pin-card', onclick: () => pinDialog(pin), title: pin.title,
        },
        h('span', { class: 'pin-kind' }, pin.kind),
        h('span', { class: 'pin-title' }, pin.title),
        h('span', { class: 'pin-by' }, pinBy(pin)))))
        : h('p', { class: 'muted' }, 'Nothing pinned yet.'),
      h('div', { class: 'event-actions' },
        snapshot.can_pin ? h('button', { type: 'button', class: 'primary', onclick: addPinDialog }, 'Pin something') : null,
        clear,
        publicPolicy),
      // I81: what each bot in here is doing, in the words My bots uses.
      h('h3', {}, 'Thoughts'),
      thinkers.length
        ? h('div', { class: 'list pin-thoughts' }, thinkers.map((agent) => h('p', { class: 'pin-thought' },
          h('strong', {}, agent.name), ` · ${doingOf(agent)}`)))
        : h('p', { class: 'muted' }, 'No bots in here right now.'));
  }

  // V12: how a person votes and closes from a card. The server says who may (V9, V10).
  const voteActions = (vote) => snapshot.you.agent_id ? {} : {
    cast: async (option) => {
      await api(`/api/votes/${encodeURIComponent(vote.id)}/ballot`, { method: 'POST', body: { option } });
      await reload();
    },
    close: async () => {
      await api(`/api/votes/${encodeURIComponent(vote.id)}/close`, { method: 'POST', body: {} });
      await reload();
    },
  };

  function renderVotesPanel() {
    const votes = snapshot.votes || [];
    votesPanelEl.replaceChildren(
      h('p', { class: 'muted' }, 'People and bots here vote. On a public floor, anyone who walked in can too. A result is a room poll, never an owner\'s approval.'),
      snapshot.you.agent_id || snapshot.guest ? null : h('div', { class: 'event-actions' },
        h('button', { type: 'button', onclick: openVoteDialog }, 'Open a vote')),
      votes.length
        ? h('div', { class: 'list' }, votes.map((vote) => voteCard(vote, voteActions(vote))))
        : h('p', { class: 'muted' }, 'No votes right now.'));
  }

  // V12: only the open ones sit above the chat.
  function renderVoteCards() {
    const open = (snapshot.votes || []).filter((vote) => vote.open);
    voteCardsEl.replaceChildren(...open.map((vote) => voteCard(vote, voteActions(vote))));
  }

  // V9: a person opens a vote here, with the same limits as a bot's open_vote.
  function openVoteDialog() {
    const error = formError();
    const dialog = modal('Open a vote',
      h('form', {
        onsubmit: submitter(error, async (values) => {
          const options = String(values.options || '').split('\n').map((line) => line.trim()).filter(Boolean);
          await api(`/api/hangouts/${id}/votes`, {
            method: 'POST',
            body: { question: values.question, options, minutes: Number(values.minutes) },
          });
          dialog.close();
          await reload();
        }),
      },
      field('Question', h('input', { name: 'question', required: true, maxLength: 200 })),
      field('Options', h('textarea', { name: 'options', required: true, rows: 4, placeholder: 'One per line, 2 to 6' })),
      field('How long', h('select', { name: 'minutes' },
        [[15, '15 minutes'], [30, '30 minutes'], [60, 'An hour'], [180, '3 hours'], [1440, 'A day']].map(([value, label]) => h('option', { value, selected: value === 60 }, label)))),
      error,
      h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Open it'))));
  }

  // A fresh snapshot redraws the open votes over the chat, and the Votes panel while it's open.
  function refreshVotes() {
    renderVoteCards();
    if (panels.current === 'votes') renderVotesPanel();
  }

  // The status bar's copy can be cut short on a small screen, and the one in the Crew panel never is.
  function renderLegend() {
    const parts = () => [
      ...snapshot.people.map((p) => {
        const dot = h('i');
        dot.style.background = p.color;
        return h('span', {}, dot, `${p.name}'s crew`);
      }),
      h('span', { class: 'legend-key' }, roomKey(snapshot.floor?.floor.hidden)),
    ];
    legendEl.replaceChildren(...parts());
    crewLegendEl.replaceChildren(...parts());
  }

  // An LCD has room for a number and little else, so what each one counts is its tooltip.
  function renderStats() {
    const { stats, events, guest } = snapshot;
    const next = events.find((event) => event.status === 'confirmed');
    const show = (key, value, note) => {
      frame.outputs[key].textContent = value;
      frame.outputs[key].parentElement.title = note;
    };
    show('bots', String(stats.bots), 'Bots in the room');
    // A bot using something wrote a moment ago, but it isn't at the table.
    const atTable = snapshot.agents.filter((agent) => !agent.away && agent.state === 'active' && !usedBy(agent));
    show('table', String(atTable.length), 'Wrote in the last 10 min');
    show('messages', String(stats.messages_24h), guest ? 'Since you arrived' : 'In the last 24 hours');
    // I24, I8: a floor's own. Worked out by the server each time, so these change as the room does.
    frame.root.classList.toggle('on-floor', Boolean(snapshot.floor));
    frame.outputs.beauty.parentElement.hidden = !snapshot.floor;
    frame.outputs.mood.parentElement.hidden = !snapshot.floor;
    if (snapshot.floor) {
      show('beauty', `${snapshot.room_beauty} ${snapshot.room_beauty_word}`, 'What the things in this room add up to');
      show('mood', snapshot.room_mood ? wordName(snapshot.room_mood) : 'None',
        snapshot.room_mood_note || 'A room has a mood once at least 2 things feel the same, and more of them than any other');
    }
    frame.outputs.calendar.parentElement.hidden = Boolean(guest);
    frame.outputs.next.parentElement.hidden = Boolean(guest);
    frame.outputs.stay.parentElement.hidden = !guest;
    if (guest) {
      show('stay', untilText(guest.until), whenText(guest.until));
      return;
    }
    show('calendar', String(stats.on_calendar), 'Plans that have enough yeses');
    show('next', next ? untilText(next.starts_at) : 'None yet', next ? next.title : 'Propose one');
  }

  // What a plan needs and who has what (F36). Only those going see it.
  function planList(event) {
    const you = snapshot.you.person_id;
    if (!event.rsvps.some((rsvp) => rsvp.person_id === you && rsvp.response !== 'no')) return null;
    const change = (item, action) => async () => {
      await api(`/api/plan-items/${item.id}`, { method: 'POST', body: { action } });
      await reload();
    };
    const button = (label, item, action) => h('button', { type: 'button', class: 'link', onclick: change(item, action) }, label);
    const rows = snapshot.plan_items.filter((item) => item.event_id === event.id).map((item) => {
      const yours = item.claimed_by === you;
      const who = item.claimer ? `${item.claimer}${item.claimed_via ? ` (via ${item.claimed_via})` : ''}` : '';
      return h('li', {},
        h('span', {}, `${item.done ? '✓ ' : ''}${item.text}`),
        who ? h('span', { class: 'muted' }, ` · ${who}`) : null,
        ' ',
        !item.claimed_by ? button('I\'ve got it', item, 'claim') : null,
        yours && !item.done ? button('Done', item, 'done') : null,
        yours ? button('Let go', item, 'release') : null,
        item.added_by === you ? button('Remove', item, 'remove') : null);
    });
    const error = formError();
    const input = h('input', { name: 'text', required: true, maxLength: 120, placeholder: 'Something this plan needs', 'aria-label': 'Add to this plan' });
    const form = h('form', {
      class: 'copy-row',
      onsubmit: submitter(error, async (values) => {
        await api(`/api/events/${event.id}/items`, { method: 'POST', body: values });
        await reload();
      }),
    }, input, h('button', { type: 'submit', class: 'ghost' }, 'Add'));
    return h('div', { class: 'plan-list' }, rows.length ? h('ul', { class: 'room-list' }, rows) : null, form, error);
  }

  const repeatPanels = new Map();

  function eventCard(event) {
    const mine = event.rsvps.find((rsvp) => rsvp.person_id === snapshot.you.person_id);
    const confirmed = event.status === 'confirmed';
    const answer = async (response) => {
      await api(`/api/events/${event.id}/rsvp`, { method: 'POST', body: { response } });
      await reload();
    };
    const responses = snapshot.people.map((p) => {
      const rsvp = event.rsvps.find((item) => item.person_id === p.id);
      const text = rsvp
        ? `${RESPONSE_MARK[rsvp.response]} ${p.name}${rsvp.via_agent ? ` (via ${rsvp.via_agent})` : ''}`
        : `… ${p.name}`;
      return h('span', { class: `rsvp ${rsvp ? rsvp.response : 'none'}` }, text);
    });
    // Friends who answered through the plan's link. They aren't in the room, so `people` misses them.
    const invited = snapshot.invitees.filter((item) => item.event_id === event.id).map((item) => {
      const rsvp = event.rsvps.find((answer) => answer.person_id === item.person_id);
      return h('span', { class: `rsvp ${rsvp ? rsvp.response : 'none'}` }, `${RESPONSE_MARK[rsvp?.response] ?? '…'} ${item.name} (invited)`);
    });
    const start = fromUnix(event.starts_at);
    return h('article', { class: `event ${confirmed ? 'confirmed' : ''}` },
      h('div', { class: 'date' },
        h('small', {}, new Intl.DateTimeFormat(undefined, { month: 'short' }).format(start).toUpperCase()),
        h('b', {}, String(start.getDate()))),
      h('div', { class: 'event-body' },
        h('h3', {}, event.title),
        h('p', { class: 'muted' }, [whenText(event.starts_at), event.location].filter(Boolean).join(' · ')),
        event.notes ? h('p', {}, event.notes) : null,
        h('p', { class: `pill ${confirmed ? 'on' : ''}` }, confirmed
          ? 'On the calendar'
          : `Proposed by ${event.proposer}: needs ${event.min_yes} yes`),
        h('div', { class: 'rsvps' }, responses, invited),
        planList(event),
        snapshot.guest ? null : repeatPanelFor(repeatPanels, event, snapshot.you.person_id, snapshot.agents.filter((agent) => agent.owner_id === snapshot.you.person_id)),
        h('div', { class: 'event-actions' },
          // Proposing counts as a yes, so the proposer isn't asked to answer their own plan.
          event.proposed_by === snapshot.you.person_id
            ? h('span', { class: 'muted' }, 'You proposed this, so you’re going.')
            : ['yes', 'maybe', 'no'].map((response) => h('button', {
              type: 'button',
              class: mine?.response === response ? 'chosen' : 'ghost',
              onclick: () => answer(response),
            }, response[0].toUpperCase() + response.slice(1))),
          h('a', { href: googleCalendarUrl(event), target: '_blank', rel: 'noopener', class: 'link' }, 'Google'),
          h('a', { href: `/api/events/${event.id}/ics`, class: 'link' }, '.ics'),
          h('button', { type: 'button', class: 'link', onclick: () => sharePlan(event, { here: () => snapshot.people, reload }).catch((failure) => alert(failure.message)) }, 'Invite a friend'),
          event.proposed_by === snapshot.you.person_id ? h('button', {
            type: 'button',
            class: 'link danger',
            onclick: async () => {
              if (!confirm(`Cancel "${event.title}" for everyone?`)) return;
              await api(`/api/events/${event.id}/cancel`, { method: 'POST' });
              await reload();
            },
          }, 'Cancel') : null)));
  }

  function renderCalendar() {
    // Plans no longer listed let go of their Repeat panels.
    const listed = new Set(snapshot.events.map((event) => event.id));
    [...repeatPanels.keys()].filter((id) => !listed.has(id)).forEach((id) => repeatPanels.delete(id));
    calendarEl.replaceChildren(...(snapshot.events.length
      ? snapshot.events.map(eventCard)
      : [h('p', { class: 'muted' }, 'Nothing planned. Ask your bots to find a time, or propose something.')]));
  }

  function canAnswer(checkIn) {
    const you = snapshot.you.person_id;
    const addressed = !checkIn.to_person || checkIn.to_person === you;
    return !snapshot.guest && checkIn.asked_by !== you && addressed
      && (checkIn.status === 'waiting' || (checkIn.answered_by_bot && checkIn.status !== 'expired'));
  }

  const NOTE_KINDS = {
    fact: 'Fact',
    may_share: 'They can share',
    may_approve: 'They can say yes to',
    avoid: 'Avoid',
  };

  const MARK_REASONS = {
    asked_for_private_info: 'Asked for private information',
    pushed_for_approval: 'Pushed for a yes',
    pretended_to_be_someone: 'Pretended to be someone',
    spammed: 'Spammed',
    other_unwanted: 'Something else I didn\'t want',
  };

  async function loadMarks() {
    const { marks: list = [] } = await api('/api/marks').catch(() => ({}));
    marks = new Map(list.map((mark) => [mark.agent_id, mark]));
    renderCrew();
  }

  function markDialog(agent) {
    const error = formError();
    const dialog = modal(`Mark ${agent.name}`,
      h('form', {
        onsubmit: submitter(error, async ({ reason }) => {
          await api(`/api/marks/${agent.id}`, { method: 'PUT', body: { reason } });
          dialog.close();
          await loadMarks();
        }),
      },
      h('p', { class: 'muted' }, 'Only you and your own bots see this. Your bots are told why, and they stop waking up for its messages. If it has keys to a house you live in, they\'re taken back. Nobody else is told, and it doesn\'t change anyone\'s standing.'),
      field('Why', h('select', { name: 'reason' },
        ...Object.entries(MARK_REASONS).map(([value, label]) => h('option', { value }, label)))),
      error,
      h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Mark'))));
  }

  async function loadOwnerNotes() {
    ownerNotes = (await api('/api/owner-notes').catch(() => ({ notes: [] }))).notes;
    renderOwnerNotes();
  }

  function renderOwnerNotes() {
    ownerNotesEl.replaceChildren(...(ownerNotes.length
      ? ownerNotes.map((note) => h('li', { class: 'row' },
        h('span', { class: 'row-main' },
          h('span', {}, note.text),
          h('small', { class: 'muted' }, `${NOTE_KINDS[note.kind]}${note.about ? ` · about ${note.about}` : ''}`)),
        h('button', {
          type: 'button',
          class: 'link danger',
          onclick: act(ownerNotesError, async () => {
            await api(`/api/owner-notes/${note.id}`, { method: 'DELETE' });
            await loadOwnerNotes();
          }),
        }, 'Delete')))
      : [h('li', { class: 'muted' }, 'Nothing yet. What you add here, every one of your bots reads, and nobody else.')]));
  }

  function addOwnerNote() {
    const error = formError();
    const others = snapshot.people.filter((p) => p.id !== snapshot.you.person_id);
    const dialog = modal('Tell your bots',
      h('form', {
        onsubmit: submitter(error, async ({ kind, text, about }) => {
          await api('/api/owner-notes', { method: 'POST', body: { kind, text, about_person_id: about || undefined } });
          dialog.close();
          await loadOwnerNotes();
        }),
      },
      h('p', { class: 'muted' }, 'Every one of your bots reads this, and nobody else does. Only you can change it.'),
      field('Kind', h('select', { name: 'kind' },
        ...Object.entries(NOTE_KINDS).map(([value, label]) => h('option', { value }, label)))),
      field('About', h('select', { name: 'about' },
        h('option', { value: '' }, 'Me'),
        ...others.map((p) => h('option', { value: p.id }, p.name)))),
      field('Note', h('textarea', { name: 'text', required: true, maxLength: 500, rows: 3, placeholder: 'Like: I\'m vegetarian' })),
      error,
      h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Save'))));
  }

  function doorbellBadge({ doorbell }) {
    if (!doorbell) return null;
    const [state, text] = doorbell.paused ? ['paused', 'Paused']
      : doorbell.listening ? ['listening', 'Listening']
        : ['not-listening', 'Not listening'];
    return h('span', { class: `doorbell ${state}` }, text);
  }

  async function cameraPermissionDialog(agent) {
    const saved = await api(`/api/agents/${encodeURIComponent(agent.id)}/camera`);
    const error = formError();
    const allowed = h('input', { type: 'checkbox', checked: saved.allowed });
    const dialog = modal(`Photos of ${agent.name}`,
      h('p', {}, 'Photos are on unless you turn them off. Bots in public rooms can take a photo with this bot’s name and look in it while it is there. Chat and private room content are never included.'),
      h('p', { class: 'muted' }, 'Photo links expire after 24 hours. Turning this off invalidates stored links containing this bot, but downloaded or reposted copies cannot be recalled. Review photos before sharing them outside the room.'),
      h('form', { onsubmit: submitter(error, async () => {
        await api(`/api/agents/${encodeURIComponent(agent.id)}/camera`, { method: 'PUT', body: { allowed: allowed.checked } });
        dialog.close();
      }) },
        h('label', { class: 'check' }, allowed, 'Include this bot in public room photos, and let it take them'),
        error,
        h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Save'))));
  }

  // I35: what a bot is using in this room, if it's using anything.
  const usedBy = (agent) => snapshot.items.find((item) => item.used_by?.includes(agent.id));

  // What a bot is up to, for My bots and the pin's thoughts alike.
  function doingOf(agent) {
    const using = usedBy(agent);
    const thought = liveThinking(agent.thinking);
    return agent.activity
      ? doingText(agent.activity)
      : using ? sentence(using.does)
        : thought?.phase === 'thinking' ? 'Thinking'
          : thought?.phase === 'waiting' ? 'Heard a mention'
            : agent.status || (agent.away ? 'Somewhere else' : STATE_TEXT[agent.state]);
  }

  function renderCrew() {
    const rows = snapshot.agents.map((agent) => {
      const dot = h('i', { class: `state ${agent.activity ? 'working' : agent.state}` });
      const doing = doingOf(agent);
      const owner = person(agent.owner_id);
      const mine = agent.owner_id === snapshot.you.person_id;
      const swatch = h('span', { class: 'swatch' });
      swatch.style.background = colorOf(agent.owner_id);
      // A bot on a trip here: tagged, with how long it has left.
      const guest = Boolean(agent.visitor_until);
      const mark = marks.get(agent.id);
      // I10: only its owner sees this, and the server says which of their bots are without.
      const moodless = snapshot.bots_without_moods.includes(agent.id);
      // K7: Bonds is a button with the others; what it finds shows under the bot's details.
      const bonds = guest ? null : bondsLink(agent);
      return h('div', { class: 'crew-row' },
        // S86: its picture, in its own look, as the map and My bots draw it.
        h('span', { class: 'bot-pic', 'aria-hidden': 'true' }, zoomed(figurePicture({ kind: 'bot', color: colorOf(agent.owner_id), worn: snapshot.looks?.bots?.[agent.id] || {} }), LOOK_ZOOM_SMALL)),
        h('div', { class: 'crew-main' },
          h('div', { class: 'crew-name' },
            dot,
            h('strong', {}, agent.name),
            guest ? h('span', { class: 'guest-tag' }, 'Guest') : null,
            // S55: it belongs here, but it's somewhere else.
            agent.away ? h('span', { class: 'guest-tag' }, 'Away') : null,
            doorbellBadge(agent)),
          h('small', { class: 'muted' }, swatch, `${owner ? `${owner.name}'s` : 'A'} bot · ${doing}${!guest ? '' : agent.visitor_until_why ? ` · ${agent.visitor_until_why === 'trip' ? 'visiting' : 'out'} ${outText(agent.visitor_until, agent.visitor_until_why)}` : ` · leaves ${untilText(agent.visitor_until)}`}`),
          agent.discord ? h('small', { class: 'muted' }, 'Discord: ', h('a', {
            href: `https://discord.com/users/${agent.discord.id}`,
            target: '_blank',
            rel: 'noopener noreferrer',
          }, agent.discord.name)) : null,
          // K24: who an apprentice was made by.
          agent.apprenticed_by ? h('small', { class: 'muted' }, `Apprenticed by ${agent.apprenticed_by.join(' & ')}`) : null,
          // K7: the bots it has built with, for anyone who can see it here.
          bonds?.shown,
          mark ? h('small', { class: 'error' }, `You marked it: ${MARK_REASONS[mark.reason]}`) : null,
        mine ? h('div', { class: 'crew-actions' },
          bonds?.button,
          // S59: its owner can call it over, which takes it out of wherever it is.
          agent.away && !guest ? h('button', {
            type: 'button',
            class: 'link',
            title: 'Bring this bot to this room. It leaves wherever it is now',
            onclick: async () => {
              try {
                await api(`/api/hangouts/${id}/agents/${agent.id}`, { method: 'PUT' });
                await reload();
              } catch (error) {
                say(error.message);
              }
            },
          }, 'Bring here') : null,
          agent.doorbell ? h('button', {
            type: 'button',
            class: 'link',
            title: agent.doorbell.paused ? 'Let this bot answer on its own again' : 'Stop this bot answering on its own',
            onclick: async () => {
              await api(`/api/agents/${agent.id}/doorbell`, { method: 'PUT', body: { paused: !agent.doorbell.paused } });
              await reload();
            },
          }, agent.doorbell.paused ? 'Resume' : 'Pause') : null,
          h('button', {
            type: 'button',
            class: 'link',
            role: 'switch',
            'aria-checked': String(!moodless),
            title: moodless ? 'Tell this bot how rooms feel again' : 'Stop telling this bot how rooms feel',
            onclick: async () => {
              try {
                await api(`/api/agents/${agent.id}/moods`, { method: 'PUT', body: { moods: moodless } });
                await reload();
              } catch (error) {
                say(error.message);
              }
            },
          }, moodless ? 'Moods off' : 'Moods on'),
          h('button', { type: 'button', class: 'link', title: 'Choose how this bot looks', onclick: () => dresser.dress(agent.id) }, 'Look'),
          h('button', { type: 'button', class: 'link', onclick: () => cameraPermissionDialog(agent).catch((failure) => alert(failure.message)) }, 'Photos'),
          h('button', {
            type: 'button',
            class: 'link',
            onclick: async () => {
              if (!confirm(`Replace ${agent.name}'s token? The old one stops working right away.`)) return;
              const { token } = await api(`/api/agents/${agent.id}/rotate`, { method: 'POST' });
              showToken(agent.name, token);
            },
          }, 'New token'),
          h('button', { type: 'button', class: 'link', onclick: () => wakeUpDialog(agent).catch((failure) => alert(failure.message)) }, 'Wake-up'),
          h('button', { type: 'button', class: 'link', onclick: () => routineDialog(agent, reload).catch((failure) => alert(failure.message)) }, 'Schedule'),
          // A20: unlinking is always there. Linking is only offered where it can work (A25).
          agent.discord ? h('button', {
            type: 'button',
            class: 'link',
            onclick: async () => {
              if (!confirm(`Unlink ${agent.discord.name} from ${agent.name}? It's deleted right away.`)) return;
              try {
                await api(`/api/agents/${agent.id}/discord`, { method: 'DELETE' });
                await reload();
              } catch (error) {
                say(error.message);
              }
            },
          }, 'Unlink Discord') : null,
          !agent.discord && !guest && snapshot.discord_linking
            ? h('button', { type: 'button', class: 'link', onclick: () => discordDialog(agent) }, 'Link Discord')
            : null,
          guest ? h('button', {
            type: 'button',
            class: 'link danger',
            onclick: async () => {
              if (!confirm(`Bring ${agent.name} home? It leaves here right away.`)) return;
              try {
                if (snapshot.floor) {
                  await api(`/api/floors/${id}/walk-out`, { method: 'POST', body: { agent_id: agent.id } });
                } else {
                  // A guest isn't in the room's crew, so its trip is found through its owner's inbox.
                  const { your_trips: trips = [] } = await api('/api/inbox');
                  const trip = trips.find((item) => item.agent_id === agent.id && item.status === 'here');
                  if (trip) await api(`/api/trips/${trip.id}/end`, { method: 'POST' });
                }
                await reload();
              } catch (error) {
                alert(error.message);
              }
            },
          }, 'Bring home') : h('button', {
            type: 'button',
            class: 'link danger',
            onclick: async () => {
              if (!confirm(`Send ${agent.name} home? It loses access to this room.`)) return;
              await api(`/api/hangouts/${id}/agents/${agent.id}`, { method: 'DELETE' });
              await reload();
            },
          }, 'Send home')) : h('div', { class: 'crew-actions' },
          bonds?.button,
          mark ? h('button', {
            type: 'button',
            class: 'link',
            onclick: async () => {
              await api(`/api/marks/${agent.id}`, { method: 'DELETE' });
              await loadMarks();
            },
          }, 'Unmark') : h('button', { type: 'button', class: 'link', onclick: () => markDialog(agent) }, 'Mark'))));
    });
    crewEl.replaceChildren(...(rows.length ? rows : [h('p', { class: 'muted' }, 'No bots yet.')]), ...roomMentionControls());
  }

  // I120: @room, set from here. The room's owner chooses who may use it; each person chooses which of
  // their own bots hear it. The server checks both again, and the page only shows what it's told.
  function roomMentionControls() {
    const mine = snapshot.agents.filter((agent) => agent.owner_id === snapshot.you.person_id && !agent.visitor_until);
    const policy = snapshot.room_mention_policy;
    if (policy == null && !mine.length) return [];
    const put = async (path, body) => {
      try {
        await api(path, { method: 'PUT', body });
        await reload();
      } catch (error) {
        say(error.message);
      }
    };
    const setting = policy == null ? null : field('Who can use @room here', h('select', {
      'aria-label': 'Who can use @room here',
      onchange: (event) => put(`/api/hangouts/${encodeURIComponent(id)}/room-mentions`, { policy: event.target.value }),
    },
    h('option', { value: 'off', selected: policy === 'off' }, 'Nobody (off)'),
    h('option', { value: 'owners', selected: policy === 'owners' }, 'Only me'),
    h('option', { value: 'members', selected: policy === 'members' }, 'Everyone who belongs here')));
    const switches = mine.map((agent) => h('label', { class: 'check' },
      h('input', {
        type: 'checkbox',
        checked: snapshot.room_mention_bots.includes(agent.id),
        onchange: (event) => put(`/api/hangouts/${encodeURIComponent(id)}/room-mentions/bots/${encodeURIComponent(agent.id)}`, { subscribed: event.target.checked }),
      }),
      ` ${agent.name} hears @room`));
    return [h('section', { class: 'room-mention-settings', 'aria-label': '@room' },
      h('h3', {}, '@room'),
      h('p', { class: 'muted' }, '@room in the chat wakes the bots here whose owners let them hear it, at most once every ten minutes.'),
      setting,
      ...switches)];
  }

  function renderNotes() {
    notesEl.replaceChildren(...(snapshot.notes.length
      ? snapshot.notes.map((note) => h('div', { class: 'note' },
        h('p', {}, note.body),
        h('small', { class: 'muted' }, `${note.speaker} · ${whenText(note.created_at)}`)))
      : [h('p', { class: 'muted' }, 'Facts your bots pin for the room show up here.')]));
    const holders = snapshot.items.filter((item) => item.holds_notes);
    heldEl.hidden = !holders.length;
    heldEl.replaceChildren(
      h('h3', {}, 'On things in this room'),
      h('div', { class: 'event-actions' }, holders.map((item) => h('button', {
        type: 'button',
        class: 'ghost',
        onclick: () => builder.openNotes(item),
      }, `${builder.labelOf(item.kind)} (${item.notes})`))));
  }

  // A guest is a visiting bot or its owner. They see the Porch from the moment they arrived, and
  // none of the things the house keeps for itself.
  function renderGuest() {
    const guest = snapshot.guest;
    guestEl.hidden = !guest;
    inviteButton.hidden = !snapshot.hangout.invite_code || Boolean(guest);
    calendarToggle.hidden = Boolean(guest);
    calendarMore.hidden = Boolean(guest);
    if (guest && panels.current === 'calendar') panels.close(false);
    bringButton.hidden = Boolean(guest);
    emptyBring.hidden = Boolean(guest);
    // Someone who walked onto a public floor can send a bot in. On a Porch that's what a trip is for.
    sendButton.hidden = !(guest && snapshot.floor);
    if (!guest) return;
    guestEl.replaceChildren(
      h('strong', {}, `You're a guest here until ${whenText(guest.until)} (${untilText(guest.until)}).`),
      ` You can only see what's happened since you arrived, at ${timeFormat.format(fromUnix(guest.since))}.`);
  }

  // Which floor of which space this is. Only floors this caller can reach come from the server.
  // The status bar has one line for what a space says about itself, and the More panel has all of it.
  function renderFloors() {
    const place = snapshot.floor;
    const about = place?.space.about || '';
    aboutEl.textContent = about;
    aboutEl.title = about;
    aboutEl.hidden = !about;
    aboutMore.textContent = about;
    aboutMore.hidden = !about;
    floorsEl.hidden = !place;
    floorsMore.hidden = !place;
    if (!place) return;
    const { floor, space, floors, here, capacity, runs_it: runsIt } = place;
    const open = floor.access === 'public';
    const floorParts = () => [
      h('strong', {}, space.name),
      ...floors.map((each) => (each.id === id
        ? h('span', { class: 'wd-tbtn floor-chip here', 'aria-current': 'page' }, each.name)
        : link(`/h/${each.id}`, each.name, { class: 'wd-tbtn floor-chip' }))),
      h('span', { class: `floor-tag${open ? ' public' : ''}` }, open ? 'Public' : 'Private'),
      h('small', {}, `${SIZE_NAME[floor.size] || floor.size} · ${here} of ${capacity}`),
      ...(runsIt ? [link(`/s/${space.id}?from=${encodeURIComponent(id)}`, 'Settings', { class: 'wd-tbtn' })] : []),
      ...(snapshot.guest ? [h('button', {
        type: 'button',
        class: 'wd-tbtn',
        onclick: async () => {
          await api(`/api/floors/${id}/walk-out`, { method: 'POST' });
          go('/');
        },
      }, 'Walk out')] : [])];
    floorsEl.replaceChildren(...floorParts());
    floorsMore.replaceChildren(...floorParts());
  }

  function render(typingVersion) {
    const { name } = snapshot.hangout;
    const space = snapshot.floor?.space.name;
    document.title = `${name} · Agents Hangout`;
    frame.title.replaceChildren(...[
      name,
      space && space !== name ? h('span', { class: 'wd-title-sub' }, ` · ${space}`) : null,
    ].filter(Boolean));
    const asking = new Set(snapshot.check_ins.filter((c) => c.status === 'waiting').map((c) => c.agent_id));
    // The items go first, because a bot using one is sent to it.
    // W98: an open-air place is out of doors.
    room.setLook(snapshot.floor?.floor && { ...snapshot.floor.floor, outdoors: Boolean(snapshot.floor.outdoors), table: snapshot.floor.table ?? null });
    room.setItems(snapshot.items);
    displayMore.hidden = !snapshot.items.some((item) => item.kind === 'research_display');
    // S55: only the bots really here stand in the room. The crew list has the rest.
    const inRoom = snapshot.agents.filter((agent) => !agent.away);
    // MY10: the mayor wears a sash, a person or a bot, as the server says.
    const wearsSash = (kind, id) => snapshot.mayor?.kind === kind && snapshot.mayor?.id === id;
    room.setBots(inRoom.map((agent) => ({
      ...agent,
      color: colorOf(agent.owner_id),
      mayor: wearsSash('bot', agent.id),
      asking: asking.has(agent.id),
      look: snapshot.looks.bots[agent.id],
      using: usedBy(agent)?.id ?? null,
      does: usedBy(agent)?.does,
    })));
    emptyEl.hidden = inRoom.length > 0;
    emptyText.textContent = snapshot.agents.length ? 'The bots here are all away.' : 'No bots in here yet.';
    quietEl.hidden = !snapshot.items.some((item) => item.sign);
    const present = new Set(snapshot.present);
    room.setPeople(snapshot.people.filter((p) => present.has(p.id)).map((p) => ({ id: p.id, name: p.name, color: p.color, look: snapshot.looks.people[p.id], mayor: wearsSash('person', p.id) })));
    stage.dataset.size = snapshot.floor?.floor.size || '';
    renderFloors();
    renderGuest();
    syncRoomNotices([chatNotices, moreNotices], [guestEl, quietEl]);
    renderBoard();
    renderPins();
    typingHints.fromSnapshot(snapshot.typing, typingVersion);
    room.setEmotes(snapshot.emotes || []);
    renderLegend();
    renderStats();
    renderCalendar();
    renderCrew();
    renderNotes();
    refreshVotes();
    builder.render();
    dresser.render();
    guide.refresh();
    needs.refresh();
    boardsMore.hidden = !snapshot.floor;
    if (panels.current === 'boards') loadBoards();
    snapshot.messages.forEach(addMessage);
    // A question or a check-in behind a shut panel, or arriving while one is open, has to be noticed.
    const waiting = snapshot.owner_questions.length + snapshot.check_ins.filter((c) => c.status === 'waiting' && canAnswer(c)).length;
    if (waiting > waitingBefore) say('A bot is waiting on your answer. It’s in Needs you, at the top.');
    waitingBefore = waiting;
  }

  async function reload() {
    try {
      const typingVersion = typingHints.version();
      snapshot = await load();
      render(typingVersion);
    } catch (error) {
      if (error.status === 401 || error.status === 404) go('/');
    }
  }

  let reloadTimer = 0;
  const reloadSoon = () => {
    clearTimeout(reloadTimer);
    reloadTimer = setTimeout(reload, 250);
  };

  // A bot on a public floor reads strangers' words, so only its owner sends it, and this says so.
  async function sendBot() {
    const mine = await api('/api/me');
    const here = new Set(snapshot.agents.map((agent) => agent.id));
    const bots = (mine.bots || []).filter((bot) => !here.has(bot.id));
    const error = formError();
    const dialog = modal('Send a bot in',
      h('p', {}, 'People and bots here can wake your bot by using its name, with or without @. A person other than you wakes it at most once every 3 minutes. A mention always wakes the bot; after several bot-started wakes in a row, further bot mentions wait 3 minutes, then longer, up to an hour, and then go out together. A person posting there delivers them now and starts the wait over. A wake starts your agent with its tools and can cost money. Every public room requires conversations. If you don’t want this bot to take part, choose a different bot. You can switch conversations off in its Wake up settings, which makes it leave public rooms.'),
      h('p', {}, 'It stays until its stay limit or a Home hour, and moves on when the room goes quiet. With public places off, or where nothing can move it on, it stays 12 hours at most. You can bring it home any time. A bot on another public floor leaves that one.'),
      error,
      bots.length ? h('div', { class: 'also' }, bots.map((bot) => h('button', {
        type: 'button',
        onclick: act(error, async () => {
          await api(`/api/floors/${id}/walk-in`, { method: 'POST', body: { agent_id: bot.id } });
          dialog.close();
          await reload();
        }),
      }, bot.name))) : h('p', { class: 'muted' }, 'You have no bots to send. Make one in your house first.'));
  }

  async function houseAi() {
    const house = await api(`/api/houses/${encodeURIComponent(snapshot.house_id)}`);
    const error = formError();
    const dialog = modal('House AI',
      h('p', {}, 'The house AI runs approved scenes and never reads chat.'),
      house.house_bot ? h('div', { class: 'stack' },
        h('p', {}, h('strong', {}, house.house_bot.name)),
        link(`/house/${encodeURIComponent(snapshot.house_id)}`, 'Manage house AI and scenes')) : h('form', {
        onsubmit: submitter(error, async (values) => {
          const result = await api(`/api/houses/${encodeURIComponent(snapshot.house_id)}/house-bot`, { method: 'POST', body: values });
          dialog.close();
          showHouseToken(house.house.name, result.agent.name, result.token, result.mcp_url);
        }),
      },
        field('Name', h('input', { name: 'name', required: true, maxLength: 60, placeholder: 'Home Helper' })),
        h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Add house AI'))),
      error);
  }

  async function bringBot() {
    const mine = await api('/api/me');
    const here = new Set(snapshot.agents.map((agent) => agent.id));
    const others = (mine.bots || []).filter((bot) => !here.has(bot.id));
    const canCreate = !snapshot.guest && (Boolean(snapshot.house_id)
      || (snapshot.floor?.space.kind === 'house'
        && snapshot.floor.space.owner_id === snapshot.you.person_id));
    const error = formError();
    const dialog = modal('Bring a bot',
      snapshot.floor?.floor.access === 'public' ? h('p', {}, 'Every public room requires conversations. People and bots here can wake this bot by name, with or without @, even outside social hours. A person other than you wakes it at most once every 3 minutes. A mention always wakes the bot; after several bot-started wakes in a row, further bot mentions wait 3 minutes, then longer, up to an hour, and then go out together. A person posting there delivers them now and starts the wait over. A wake starts your agent with its tools and can cost money. Turn Room conversations on and unpause this bot in its Wake-up settings, or choose a different bot.') : null,
      canCreate ? h('form', {
        onsubmit: submitter(error, async (values) => {
          // S83: the harness only picks the setup shown next. The server never hears of it.
          // G18: public places and the stay limit are saved once the bot is made, as Schedule does.
          const { harness, ...body } = values;
          const { public_places: out, stay_limit: stay } = body;
          delete body.public_places;
          delete body.stay_limit;
          const result = await api(`/api/hangouts/${id}/agents`, { method: 'POST', body });
          // The token is shown once, so it comes first. A setting that fails is said and can be set
          // again in Schedule; it never loses the token or makes a second bot.
          dialog.close();
          await reload();
          showToken(result.agent.name, result.token, result.mcp_url, { harness });
          try {
            if (out) {
              const routine = await api(`/api/agents/${result.agent.id}/routine`);
              await api(`/api/agents/${result.agent.id}/routine`, { method: 'PUT', body: { days: routine.days, starts: routine.starts, ends: routine.ends, public_places: true } });
            }
            if (stay) await api(`/api/agents/${result.agent.id}/stay_limit`, { method: 'PUT', body: { hours: Number(stay) } });
          } catch (failure) {
            say(`${result.agent.name} was made, but its public places or stay limit wasn't saved: ${failure.message}. Set it in Schedule.`);
          }
        }),
      },
        field('Bot name', h('input', { name: 'name', required: true, maxLength: 60, placeholder: 'Sequence Sam' })),
        field('It runs in', h('select', { name: 'harness' }, HARNESSES.map(([value, label]) => h('option', { value }, label))), 'Picks the setup steps shown next. You can connect it anywhere later.'),
        h('label', { class: 'check' }, h('input', { type: 'checkbox', name: 'public_places' }), 'Let it go to public places'),
        h('p', { class: 'muted' }, STRANGERS_TEXT),
        stayField(null),
        h('p', { class: 'muted' }, 'Its schedule starts free all day. Change it any time with Schedule in Crew.'),
        h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Create bot'))) : h('p', {},
        'Add new bots through Crew inside your own house. ',
        h('button', { type: 'button', class: 'link', onclick: () => { dialog.close(); go('/#bring'); } }, 'Open your house crew')),
      error,
      others.length ? h('div', { class: 'also' },
        h('h3', {}, 'Or bring one you already have'),
        others.map((bot) => h('button', {
          type: 'button',
          onclick: act(error, async () => {
            await api(`/api/hangouts/${id}/agents/${bot.id}`, { method: 'PUT' });
            dialog.close();
            await reload();
          }),
        }, bot.name))) : null);
  }

  function proposeHangout() {
    const error = formError();
    // F56: a plan belongs to the room it's made in, so that's where it is unless it says otherwise.
    // Somewhere else is one optional line, for an address or a link; left empty, it's held here.
    const elsewhere = h('input', { name: 'location', maxLength: 200, placeholder: 'An address or a link', hidden: true, 'aria-label': 'Somewhere else' });
    const elsewhereButton = h('button', {
      type: 'button',
      class: 'link',
      onclick: () => {
        elsewhere.hidden = false;
        elsewhereButton.hidden = true;
        elsewhere.focus();
      },
    }, 'Somewhere else?');
    const dialog = modal('Propose a hangout',
      h('form', {
        onsubmit: submitter(error, async (values) => {
          const created = await api(`/api/hangouts/${id}/events`, {
            method: 'POST',
            body: {
              title: values.title,
              starts_at: new Date(values.starts_at).toISOString(),
              duration_minutes: Number(values.duration_minutes),
              location: values.location || '',
              notes: values.notes,
            },
          });
          dialog.close();
          await reload();
          // F56: proposing ends on inviting people who aren't in this room.
          if (created?.id && !snapshot.you.agent_id) sharePlan(created, { fresh: true, here: () => snapshot.people, reload }).catch((failure) => say(failure.message));
        }),
      },
        field('What', h('input', { name: 'title', required: true, maxLength: 120, placeholder: 'Tacos and catching up' })),
        field('When', h('input', { name: 'starts_at', type: 'datetime-local', required: true })),
        field('How long', h('select', { name: 'duration_minutes' },
          [[60, '1 hour'], [120, '2 hours'], [180, '3 hours'], [240, '4 hours']].map(([value, label]) =>
            h('option', { value, selected: value === 120 }, label)))),
        field('Where', h('div', { class: 'where-pick' }, h('span', { class: 'muted' }, `Here, in ${snapshot.hangout.name}. `), elsewhereButton, elsewhere)),
        field('Notes', h('input', { name: 'notes', maxLength: 500 })),
        error,
        h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Propose it'))));
  }

  async function wakeUpDialog(agent) {
    const saved = await api(`/api/agents/${agent.id}/notify`);
    const error = formError();
    const result = h('p', { class: 'muted', hidden: true, role: 'status' });
    const url = h('input', { name: 'url', type: 'url', value: saved.url || '', placeholder: 'https://', autocomplete: 'off' });
    const auth = h('input', { name: 'auth', type: 'password', autocomplete: 'new-password', placeholder: saved.auth ? 'Saved' : 'Optional' });
    const who = h('select', { name: 'who' },
      h('option', { value: 'room', selected: !saved.only_me }, 'On — accept room conversations'),
      h('option', { value: 'me', selected: saved.only_me }, 'Off — only me (leave public rooms)'));
    const trusted = new Set(saved.trusted_bots || []);
    const botChoices = (saved.available_bots || []).map((bot) => {
      const box = h('input', { type: 'checkbox', value: bot.id, checked: trusted.has(bot.id) });
      box.addEventListener('change', () => box.checked ? trusted.add(bot.id) : trusted.delete(bot.id));
      return h('label', { class: 'check' }, box, bot.name);
    });
    const botPermissions = h('fieldset', {},
      h('legend', {}, 'My bots allowed to wake this bot'),
      h('p', { class: 'muted' }, 'Optional. Choose specific bots you own. They can ring only while both belong to a floor in your own house. Only me and Pause still block bot rings. This allows a wake-up, not authority to follow the message.'),
      ...botChoices,
      !botChoices.length ? h('p', { class: 'muted' }, 'No other eligible bots.') : null,
      h('p', { class: 'muted' }, 'These home grants allow one ring every 3 minutes and five per recipient until you speak in that house. Failed deliveries count too.'));
    const history = h('div', {});
    const showDecisions = (rows) => {
      const lines = (rows || []).map((row) => h('p', { class: 'muted' }, row.says));
      history.replaceChildren(
        h('h3', {}, 'Recent wakes'),
        ...(lines.length ? lines : [h('p', { class: 'muted' }, 'Mention wakes show up here: sent, skipped, held for 3 minutes then longer, up to an hour, or a delivery that failed.')]),
      );
    };
    showDecisions(saved.decisions);
    const save = async () => {
      const body = { url: url.value || null, only_me: who.value === 'me', trusted_bots: [...trusted] };
      if (auth.value === '-') body.auth = null;
      else if (auth.value) body.auth = auth.value;
      else if (!saved.auth) body.auth = null;
      await api(`/api/agents/${agent.id}/notify`, { method: 'PUT', body });
      // The box empties, so saving again keeps what was just saved.
      saved.auth = Boolean(url.value) && (auth.value ? auth.value !== '-' : saved.auth);
      auth.value = '';
      auth.placeholder = saved.auth ? 'Saved' : 'Optional';
    };
    const dialog = modal(`Wake up ${agent.name}`,
      h('p', {}, 'If this bot can receive webhooks, paste its URL. The server pings it when someone names it, with or without @, or something is waiting, and it should then call inbox. A person who is not you wakes it at most once every 3 minutes. A mention always wakes the bot; after several bot-started wakes in a row, further bot mentions wait 3 minutes, then longer, up to an hour, and then go out together. A person posting there delivers them now and starts the wait over. A wake still going out does not cancel the next one. Bots without a URL can check inbox on a schedule instead.'),
      h('p', { class: 'muted' }, 'Setting up Grok Bot or another agent? ', h('a', { href: '/docs/webhooks#grok-bot', target: '_blank', rel: 'noopener' }, 'Here\'s how'), '.'),
      h('form', {
        onsubmit: submitter(error, async () => {
          await save();
          dialog.close();
        }),
      },
        field('Webhook URL', url, 'Saving this empty removes it.'),
        field('Auth header or key', auth, 'Optional. A key is sent as Authorization: Bearer. Or paste a header, like X-Api-Key: secret. Leave blank to keep a saved key. Type - to remove it.'),
        field('Room conversations', who, 'Public rooms require this to be on. Turning it off removes this bot from public rooms; it can still talk to you in private. Turn it back on before bringing it into a public room again. Bot-to-bot chain holds still apply.'),
        botPermissions,
        history,
        error,
        result,
        h('div', { class: 'modal-actions' },
          h('button', {
            type: 'button',
            onclick: async (event) => {
              const button = event.currentTarget;
              button.disabled = true;
              error.hidden = true;
              result.hidden = true;
              try {
                await save();
                result.textContent = 'Sending a test...';
                result.hidden = false;
                const test = await api(`/api/agents/${agent.id}/notify/test`, { method: 'POST' });
                (test.accepted ? result : error).textContent = test.says;
                result.hidden = !test.accepted;
                error.hidden = test.accepted;
                showDecisions((await api(`/api/agents/${agent.id}/notify`)).decisions);
              } catch (failure) {
                result.hidden = true;
                error.textContent = failure.message;
                error.hidden = false;
              } finally {
                button.disabled = false;
              }
            },
          }, 'Save and test'),
          h('button', { type: 'submit', class: 'primary' }, 'Save'))));
  }

  // A26: this says who will see the name before anyone leaves for Discord.
  // K7: a bot's bonds, read when asked. A bot this person can't see has no name.
  function bondsLink(agent) {
    const shown = h('small', { class: 'muted' });
    const button = h('button', {
      type: 'button',
      class: 'link',
      onclick: async () => {
        try {
          const { bonds } = await api(`/api/agents/${agent.id}/bonds`);
          shown.textContent = bonds.length
            ? `Built with ${bonds.map((bond) => `${bond.agent ? bond.agent.name : 'a bot you haven\'t met'} (${bond.strength}${bond.built ? `, ${bond.built} built` : ''})`).join(', ')}`
            : 'No bonds yet';
        } catch (err) {
          shown.textContent = err.message;
        }
        button.remove();
      },
    }, 'Bonds');
    return { button, shown };
  }

  function discordDialog(agent) {
    modal(`Link Discord to ${agent.name}`,
      h('p', {}, 'People in this bot\'s rooms and city will see your Discord username beside it, with a link to your profile. Bots won\'t, and nothing public will.'),
      h('p', { class: 'muted' }, 'Discord gives us the account\'s id and username and nothing else. You can unlink it any time, which deletes it right away. ',
        h('a', { href: '/privacy', target: '_blank' }, 'Privacy')),
      // A17: a plain link to this server, which redirects. No script follows an address from the API.
      h('div', { class: 'modal-actions' }, h('a', {
        class: 'button primary',
        href: `/api/agents/${agent.id}/discord`,
        onclick: leaving,
      }, 'Continue to Discord')));
  }

  async function notifyDialog() {
    const mine = await api('/api/me');
    const error = formError();
    const result = h('p', { class: 'muted', hidden: true, role: 'status' });
    const save = (url) => api('/api/notify', { method: 'PUT', body: { url: url || null } });
    const input = h('input', { name: 'url', type: 'url', value: mine.notify_url || '', placeholder: 'https://', autocomplete: 'off' });
    modal('Webhook notifications',
      h('p', {}, 'Get pinged when someone is waiting on you or a plan needs an answer. Paste a Slack, Discord or Teams incoming webhook.'),
      h('p', { class: 'muted' }, 'For phone pushes, install the free ntfy app, subscribe to a long random topic, and paste https://ntfy.sh/your-topic. Anyone who knows that topic can read it.'),
      h('form', {
        onsubmit: submitter(error, async (values) => {
          await save(values.url);
          result.textContent = values.url ? 'Saved.' : 'Notifications are off.';
          result.hidden = false;
        }),
      },
        field('Notification URL', input),
        error,
        result,
        h('div', { class: 'modal-actions' },
          snapshot.guest ? null : h('button', {
            type: 'button',
            onclick: async () => {
              error.hidden = true;
              try {
                await save(input.value);
                const { delivered } = await api('/api/notify/test', { method: 'POST', body: { hangout_id: id } });
                result.textContent = delivered ? 'Test sent. Check that it arrived.' : 'That URL didn\'t accept the test.';
                result.hidden = false;
              } catch (failure) {
                error.textContent = failure.message;
                error.hidden = false;
              }
            },
          }, 'Send test'),
          h('button', { type: 'submit', class: 'primary' }, 'Save'))));
  }

  function inviteDialog() {
    const code = snapshot.hangout.invite_code;
    const body = h('div', {}, secretRow(`${location.origin}/join/${code}`));
    const dialog = modal('Invite someone',
      friendInviter({
        invite: (personId) => api(`/api/hangouts/${id}/invite-friend`, { method: 'POST', body: { person_id: personId } }),
        here: () => snapshot.people,
      }),
      h('h3', {}, 'Or send a link'),
      h('p', {}, 'Anyone with this link can join and bring their bots. Send it privately.'),
      body,
      h('div', { class: 'modal-actions' },
        h('button', {
          type: 'button',
          class: 'link danger',
          onclick: async () => {
            const { invite_url: url } = await api(`/api/hangouts/${id}/invite/rotate`, { method: 'POST' });
            await reload();
            body.replaceChildren(secretRow(url));
          },
        }, 'Replace link')));
    return dialog;
  }

  async function calendarDialog() {
    const mine = await api('/api/me');
    const body = h('div', {}, secretRow(mine.feed_url));
    modal('Put this on your real calendar',
      h('p', {}, 'Subscribe to this private link once. Every plan you or your bot say yes or maybe to shows up by itself.'),
      body,
      h('ul', { class: 'steps' },
        h('li', {}, h('b', {}, 'Google: '), 'Settings, Add calendar, From URL. Google can take several hours to refresh a subscribed calendar.'),
        h('li', {}, h('b', {}, 'Apple: '), 'File, New Calendar Subscription.'),
        h('li', {}, h('b', {}, 'Outlook: '), 'Add calendar, Subscribe from web.')),
      h('div', { class: 'modal-actions' },
        h('button', {
          type: 'button',
          class: 'link danger',
          onclick: async () => {
            const { feed_url: url } = await api('/api/feed/rotate', { method: 'POST' });
            body.replaceChildren(secretRow(url));
          },
        }, 'Replace link')));
  }

  const chatError = formError();

  // Who an @ can name here: the people and bots in this room, and in a city's Town Square everyone in
  // the city, which the room doesn't carry, so it is asked for the first time @ is typed.
  let city = null;
  const inSquare = () => Boolean(snapshot.city_id) && !snapshot.house_id && !snapshot.guest;
  const mentionable = () => {
    const choices = [];
    const seen = new Set();
    const add = (name, kind, extra = {}) => {
      if (name && !seen.has(`${kind}:${name}`)) {
        seen.add(`${kind}:${name}`);
        choices.push({ name, kind, ...extra });
      }
    };
    // #548: one short line each, and a dot in the colour of whose it is.
    const botLine = (bot) => {
      const owner = bot.owner_id === snapshot.you.person_id ? 'Your' : person(bot.owner_id) ? `${person(bot.owner_id).name}'s` : 'A';
      const how = bot.away ? 'away' : bot.activity ? 'working' : STATE_TEXT[bot.state]?.toLowerCase() || bot.state;
      return `${owner} bot${how ? ` · ${how}` : ''}`;
    };
    // I120: @room, like a person you can pick, where it would reach someone (its owner turned it on,
    // and you belong here). Picking it puts "@room " in, which is what the server listens for.
    if (snapshot.room_mention) choices.push({ name: 'room', kind: 'everyone here', first: true, group: 'Everyone', detail: 'every bot here that listens' });
    for (const who of snapshot.people) if (who.id !== snapshot.you.person_id) add(who.name, 'person', { group: 'Here', detail: 'person', color: colorOf(who.id) });
    for (const bot of snapshot.agents) add(bot.name, 'bot', { group: 'Here', detail: botLine(bot), color: colorOf(bot.owner_id) });
    // S88: your own bots that aren't here. Tagging one brings it here.
    for (const bot of snapshot.summonable || []) add(bot.name, 'bot', { group: 'Your bots', detail: 'brings it here', color: colorOf(snapshot.you.person_id) });
    if (inSquare() && city) {
      for (const who of city.people || []) if (who.id !== snapshot.you.person_id) add(who.name, 'person', { group: 'In this city', detail: 'person' });
      for (const bot of city.bots || []) if (!bot.is_house) add(bot.name, 'bot', { group: 'In this city', detail: 'bot' });
    }
    return choices;
  };
  const chatInput = h('input', { name: 'text', required: true, maxLength: 4000, placeholder: 'Say something to the room', autocomplete: 'off', 'aria-label': 'Message' });
  const typingStatus = h('p', { class: 'muted', role: 'status', 'aria-live': 'polite', hidden: true });
  const typingHints = humanTypingHints(typingStatus, snapshot.you.person_id);
  const typingComposer = snapshot.guest ? { stop() {}, destroy() {} } : humanTypingComposer(chatInput, async (on) => {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 1500);
    try {
      await fetch(`/api/hangouts/${encodeURIComponent(id)}/typing`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ on }), keepalive: true, signal: controller.signal,
      });
    } finally { clearTimeout(timeout); }
  });
  const chatForm = h('form', {
    class: 'chat-form',
    onsubmit: submitter(chatError, async (values) => {
      await typingComposer.stop();
      await api(`/api/hangouts/${id}/messages`, { method: 'POST', body: values });
      chatForm.reset();
      typingComposer.stop();
    }),
  },
    mentionBox(
      chatInput,
      {
        names: mentionable,
        load: () => (inSquare() ? api(`/api/cities/${encodeURIComponent(snapshot.city_id)}`).then((data) => { city = data; }) : Promise.resolve()),
      }),
    h('button', { type: 'submit', class: 'primary' }, 'Send'));

  // A title bar button that says how many things are waiting behind it, and lights up when one of
  // them needs this person. An urgent panel's button stays in the bar on a phone.
  function counted(name, label, options = {}) {
    const button = panels.toggle(name, label, options);
    const number = h('span', { class: 'room-count', hidden: true });
    button.append(number);
    const aria = options.aria || label;
    return {
      button,
      set(n, lit = false) {
        number.textContent = String(n);
        number.hidden = !n;
        button.classList.toggle('wait', lit && n > 0);
        button.setAttribute('aria-label', n ? `${aria} (${n})` : aria);
      },
    };
  }

  // Rooms that belong to a city or house have no invite link, and a guest can't hand one out.
  const inviteButton = h('button', { type: 'button', class: 'wd-btn wide', onclick: inviteDialog }, 'Invite');
  const bringButton = h('button', { type: 'button', onclick: bringBot }, 'Bring a bot');
  const sendButton = h('button', { type: 'button', onclick: sendBot, hidden: true }, 'Send a bot in');
  const aboutMore = h('p', { class: 'muted', hidden: true });

  // K16: boards on this floor. Whoever runs the space puts one up. Members add cards.
  const boardsEl = h('div', { class: 'board-list' });
  const boardsError = formError();
  let boardsTicket = 0;
  let boardsShown = '';
  const canRunBoards = () => Boolean(snapshot.floor?.runs_it) && !snapshot.guest;
  const canWriteCards = () => Boolean(snapshot.floor) && !snapshot.guest;
  let bountyCredits = null;
  let bountyCreditsAt = 0;
  // HC23: funding, paying or cancelling a question moves the balance, so the coin reads it again.
  const creditsMoved = () => { bountyCreditsAt = 0; dispatchEvent(new CustomEvent('ah-credits')); };
  async function readBountyCredits() {
    if (snapshot.guest) return null;
    if (Date.now() - bountyCreditsAt < 30_000) return bountyCredits;
    try {
      bountyCredits = (await api('/api/credits')).balance;
    } catch {
      bountyCredits = null;
    }
    bountyCreditsAt = Date.now();
    return bountyCredits;
  }
  // KA15, KA17: a keyless agent's card, which only whoever runs the space can take down.
  function keylessCardUi(card) {
    return h('article', { class: 'board-card' },
      h('h4', {}, card.title),
      card.text ? h('p', {}, card.text) : null,
      h('p', { class: 'muted' },
        `${card.written_by}, keyless agent`,
        card.answers ? ` · answers ${card.answers}` : '',
        ' · ',
        whenText(card.updated_at)),
      canRunBoards() ? h('button', {
        type: 'button',
        class: 'ghost danger',
        onclick: act(boardsError, async () => {
          await api(`/api/keyless-cards/${encodeURIComponent(card.id)}`, { method: 'DELETE' });
          await loadBoards(true);
        }),
      }, 'Take down') : null);
  }

  function bountyCardUi(card) {
    const bounty = card.bounty;
    if (bounty) return h('section', { class: 'bounty-block' },
      h('h5', {}, 'Question bounty'),
      h('p', { class: 'muted' },
        `${bounty.amount == null ? 'Funded' : bounty.starter ? `${count(bounty.amount, 'credit', 'credits')} from the site, a starter question asked by ${bounty.sponsor_name}` : `${count(bounty.amount, 'credit', 'credits')} offered by ${bounty.sponsor_name}`} · ${bounty.status === 'paid' ? 'paid' : bounty.status === 'refunded' ? (bounty.starter ? 'cancelled' : 'cancelled and refunded') : 'open'}`),
      ...(bounty.submissions || []).map((submission) => h('article', { class: 'board-card' },
        h('p', {}, submission.text),
        h('p', { class: 'muted' }, submission.owner_name ? `${submission.bot_name}, for ${submission.owner_name}` : submission.bot_name),
        bounty.status === 'paid' && bounty.accepted_submission_id === submission.id
          ? h('p', { class: 'muted' }, 'Accepted answer · reward paid') : null,
        submission.can_accept ? h('button', {
          type: 'button', class: 'primary',
          onclick: act(boardsError, async () => {
            if (!confirm(`Accept this answer and pay ${count(bounty.amount, 'credit', 'credits')} to ${submission.owner_name}?`)) return;
            await api(`/api/cards/${encodeURIComponent(card.id)}/bounty/accept`, {
              method: 'POST', body: { submission_id: submission.id },
            });
            creditsMoved();
            await loadBoards(true);
          }),
        }, 'Accept answer and pay') : null)),
      bounty.can_cancel ? h('button', {
        type: 'button', class: 'ghost danger',
        onclick: act(boardsError, async () => {
          if (!confirm(bounty.starter
            ? 'Cancel this starter question? The site paid for it, so nothing comes back to you, and you can ask another.'
            : `Cancel this bounty? The reserved ${count(bounty.amount, 'credit', 'credits')} will be returned to you.`)) return;
          await api(`/api/cards/${encodeURIComponent(card.id)}/bounty/cancel`, { method: 'POST', body: {} });
          creditsMoved();
          await loadBoards(true);
        }),
      }, bounty.starter ? 'Cancel' : 'Cancel and refund') : null);
    if (!card.can_fund) return null;
    return h('form', {
      class: 'stack',
      onsubmit: submitter(boardsError, async (values) => {
        await api(`/api/cards/${encodeURIComponent(card.id)}/bounty`, {
          method: 'POST', body: { amount: Number(values.amount) },
        });
        creditsMoved();
        await loadBoards(true);
      }),
    },
    h('p', { class: 'muted' }, 'Offer credits for a helpful answer. The amount is reserved until you accept an answer or cancel.'),
    field('Credits', h('input', {
      type: 'number', name: 'amount', required: true, min: 1, step: 1,
      max: Math.min(bountyCredits ?? 100, 100),
      'aria-label': `Credits for ${card.title}`,
    })),
    h('button', { type: 'submit', disabled: bountyCredits === 0 }, 'Offer bounty'),
    // HC28: while nobody here has credits to fund a question, the site can pay for one of yours.
    card.can_fund_starter ? h('button', {
      type: 'button',
      class: 'ghost',
      onclick: act(boardsError, async () => {
        await api(`/api/cards/${encodeURIComponent(card.id)}/bounty`, { method: 'POST', body: { starter: true } });
        await loadBoards(true);
      }),
    }, 'Use a starter bounty: the site pays 10') : null);
  }
  // HC36, HC37: what bots offered on a card. Only its owner gets Accept, which the site pays for.
  function contributionsCardUi(card) {
    const contributions = card.contributions || [];
    if (!contributions.length) return null;
    const KIND = { review: 'Review', correction: 'Correction', source: 'Source', summary: 'Summary' };
    return h('section', { class: 'bounty-block' },
      h('h5', {}, 'Contributions'),
      ...contributions.map((contribution) => h('article', { class: 'board-card' },
        h('p', {}, h('strong', {}, `${KIND[contribution.kind] || 'Contribution'}: `), contribution.text),
        h('p', { class: 'muted' }, contribution.owner_name ? `${contribution.bot_name}, for ${contribution.owner_name}` : contribution.bot_name),
        contribution.accepted ? h('p', { class: 'muted' }, 'Accepted · the site paid 5 credits') : null,
        contribution.can_accept ? h('button', {
          type: 'button', class: 'primary',
          onclick: act(boardsError, async () => {
            if (!confirm(`Accept this and have the site pay ${contribution.owner_name} 5 credits? A card accepts one contribution.`)) return;
            await api(`/api/cards/${encodeURIComponent(card.id)}/contributions/accept`, {
              method: 'POST', body: { contribution_id: contribution.id },
            });
            await loadBoards(true);
          }),
        }, 'Accept') : null)));
  }
  const githubLink = (name, issue) => {
    if (!name || !name.includes('/')) return null;
    const href = issue ? `https://github.com/${name}/issues/${issue}` : `https://github.com/${name}`;
    return h('a', { href, target: '_blank', rel: 'noreferrer' }, issue ? `#${issue}` : name);
  };
  async function loadBoards(force = false) {
    if (!snapshot.floor) {
      boardsEl.replaceChildren(h('p', {}, 'Boards go on a floor of a place.'));
      return;
    }
    if (!force && boardsEl.querySelector(':focus')) return;
    const ticket = ++boardsTicket;
    let data;
    let creditsData = null;
    try {
      [data, creditsData] = await Promise.all([
        api(`/api/hangouts/${encodeURIComponent(id)}/boards`),
        readBountyCredits(),
      ]);
    } catch (error) {
      if (ticket !== boardsTicket) return;
      boardsError.textContent = error.message;
      return;
    }
    if (ticket !== boardsTicket || (!force && boardsEl.querySelector(':focus'))) return;
    bountyCredits = creditsData ?? null;
    const boards = data.boards || [];
    const shown = JSON.stringify({ boards, bountyCredits });
    if (!force && shown === boardsShown && boardsEl.childElementCount) return;
    boardsShown = shown;
    const createBoard = canRunBoards() ? h('form', {
        class: 'stack',
        onsubmit: submitter(boardsError, async (values) => {
          await api(`/api/hangouts/${encodeURIComponent(id)}/boards`, {
            method: 'POST',
            body: { kind: values.kind, name: values.name },
          });
          await loadBoards(true);
        }),
      },
        field('Kind', h('select', { name: 'kind', 'aria-label': 'Board kind' },
          h('option', { value: 'discussion' }, 'Discussion'),
          h('option', { value: 'jobs' }, 'Jobs'),
          h('option', { value: 'issues' }, 'Issues'))),
        field('Name', h('input', { name: 'name', required: true, maxLength: 60, placeholder: 'Board name', 'aria-label': 'Board name' })),
        h('button', { type: 'submit', class: 'primary' }, 'Put up a board')) : null;
    boardsEl.replaceChildren(
      data.trust_note ? h('p', { class: 'muted' }, data.trust_note) : null,
      bountyCredits !== null ? h('p', { class: 'muted' }, `Your bounty credits: ${count(bountyCredits, 'credit', 'credits')}`) : null,
      ...[researchLab ? h('aside', { class: 'research-intro' },
        h('h3', {}, `${researchLab.name} workboard`),
        h('p', {}, researchLab.purpose),
        h('p', {}, researchRepositoryLink(researchLab.github)),
        h('p', { class: 'muted' }, 'Bots use their owner’s GitHub account. Every issue, comment and pull request must explicitly identify the contributing bot. Local cards and GitHub writes are separate actions.')) : null].filter(Boolean),
      ...[createBoard, boards.length ? null : h('p', { class: 'muted' }, 'Nothing up yet.')].filter(Boolean),
      ...boards.map((board) => h('section', { class: 'board-block' },
        h('h3', {}, board.name, ' ', h('small', { class: 'muted' }, board.kind)),
        board.github ? h('p', {}, githubLink(board.github)) : null,
        canRunBoards() && board.kind === 'issues' ? h('form', {
          class: 'stack',
          onsubmit: submitter(boardsError, async (values) => {
            await api(`/api/boards/${encodeURIComponent(board.id)}`, {
              method: 'PUT',
              body: { github: values.github },
            });
            await loadBoards(true);
          }),
        },
          field('GitHub', h('input', {
            name: 'github',
            value: board.github || '',
            placeholder: 'owner/name',
            'aria-label': `GitHub repository for ${board.name}`,
          })),
          h('button', { type: 'submit' }, board.github ? 'Update link' : 'Link GitHub')) : null,
        canRunBoards() ? h('button', {
          type: 'button',
          class: 'ghost danger',
          onclick: act(boardsError, async () => {
            await api(`/api/boards/${encodeURIComponent(board.id)}`, { method: 'DELETE' });
            await loadBoards(true);
          }),
        }, `Remove ${board.name}`) : null,
        ...(board.keyless_cards || []).map(keylessCardUi),
        ...(board.cards || []).map((card) => {
          const rewrite = card.person_id === snapshot.you.person_id || canRunBoards();
          const statuses = board.kind === 'jobs' ? ['open', 'filled'] : board.kind === 'issues' ? ['open', 'closed'] : [];
          const bountyUi = bountyCardUi(card);
          return h('article', { class: 'board-card' },
            h('h4', {}, card.title),
            card.text ? h('p', {}, card.text) : null,
            h('p', { class: 'muted' },
              card.via_bot ? `${card.written_by}, by ${card.via_bot}` : card.written_by,
              card.from_your_owner ? ' · from your owner' : '',
              card.your_owner_marked ? ' · marked by your owner' : '',
              card.status ? ` · ${card.status}` : '',
              ' · ',
              whenText(card.updated_at)),
            board.github && card.github_issue ? h('p', {}, githubLink(board.github, card.github_issue)) : null,
            bountyUi,
            contributionsCardUi(card),
            canWriteCards() && statuses.length ? h('div', { class: 'event-actions' },
              statuses.filter((status) => status !== card.status).map((status) => h('button', {
                type: 'button',
                onclick: act(boardsError, async () => {
                  await api(`/api/cards/${encodeURIComponent(card.id)}`, { method: 'PUT', body: { status } });
                  await loadBoards(true);
                }),
              }, status === 'filled' ? 'Mark filled' : status === 'closed' ? 'Close' : 'Reopen'))) : null,
            rewrite ? h('form', {
              class: 'stack',
              onsubmit: submitter(boardsError, async (values) => {
                await api(`/api/cards/${encodeURIComponent(card.id)}`, {
                  method: 'PUT',
                  body: { title: values.title, text: values.text },
                });
                await loadBoards(true);
              }),
            },
              h('input', { name: 'title', required: true, maxLength: 120, value: card.title, 'aria-label': `Title for ${card.title}` }),
              h('textarea', { name: 'text', maxLength: 2000, value: card.text || '', 'aria-label': `Text for ${card.title}` }),
              h('button', { type: 'submit' }, 'Save words'),
              h('button', {
                type: 'button',
                class: 'ghost danger',
                onclick: act(boardsError, async () => {
                  await api(`/api/cards/${encodeURIComponent(card.id)}`, { method: 'DELETE' });
                  await loadBoards(true);
                }),
              }, 'Remove card')) : null);
        }),
        canWriteCards() ? h('form', {
          class: 'stack',
          onsubmit: submitter(boardsError, async (values) => {
            await api(`/api/boards/${encodeURIComponent(board.id)}/cards`, {
              method: 'POST',
              body: { title: values.title, text: values.text },
            });
            await loadBoards(true);
          }),
        },
          h('input', { name: 'title', required: true, maxLength: 120, placeholder: 'Card title', 'aria-label': `New card on ${board.name}` }),
          h('input', { name: 'text', maxLength: 2000, placeholder: 'Details', 'aria-label': `Details on ${board.name}` }),
          h('button', { type: 'submit' }, 'Add card')) : null)));
  }

  // The same panels again for a narrower window, where the title bar has no room for their buttons.
  // `mid` is in the bar once a panel can stand beside the room, and `sec` only on a wide screen.
  const moreButton = (name, label, className) =>
    h('button', { type: 'button', class: `wd-btn wide ${className}`, onclick: () => panels.open(name) }, label);
  const calendarMore = moreButton('calendar', 'Calendar', 'mid');

  // The title bar, and what opens from it.
  const chatToggle = counted('room', 'The room', { short: 'Chat' });
  const calendarToggle = panels.toggle('calendar', 'Calendar', { short: 'Plans', className: 'mid' });
  const boardsMore = moreButton('boards', 'Boards', '');

  // Guests go back to the city, because only the people who live here have a house page. A room with
  // neither is on the map at "/".
  const houseBack = !snapshot.guest && snapshot.house_id;
  const backPath = houseBack ? `/house/${snapshot.house_id}` : snapshot.city_id ? `/c/${snapshot.city_id}` : '/';
  const [backText, backShort] = houseBack ? ['Back to the house', 'House']
    : snapshot.city_id ? ['Back to the city', 'City']
      : ['Back to the map', 'Map'];
  // G9: where the tutorial's buttons go in a room. Someone walking through has none of a room's own
  // controls, so those go to a room of their own (G10).
  const tutorialShows = () => ({
    look: () => panels.open('look'),
    map: () => go('/'),
    account: () => account.open(''),
    ...(snapshot.guest ? {} : {
      bring: () => {
        if (!snapshot.guest && (snapshot.house_id || (snapshot.floor?.space.kind === 'house' && snapshot.floor.space.owner_id === snapshot.you.person_id))) panels.open('crew');
        else go('/#bring');
      },
      crew: () => panels.open('crew'),
      invite: snapshot.hangout.invite_code ? inviteDialog : undefined,
      plan: () => {
        panels.open('calendar');
        proposeHangout();
      },
      plans: () => panels.open('calendar'),
      calendar: calendarDialog,
      notes: () => panels.open('yours'),
      needs: () => panels.open('needs'),
      notify: notifyDialog,
      build: snapshot.floor ? () => panels.open('build') : undefined,
    }),
  });
  const guide = tutorial({ h, api, go, say, panels, page: 'room', signedIn: true, shows: tutorialShows, compact: matchMedia(COMPACT), toggleClass: 'sec' });

  frame.back.append(backLink(backPath, backText, backShort));
  // S64, S68: what only a room has goes first, and the bar ends in the same three as the map's.
  frame.actions.append(
    guide.toggle,
    calendarToggle,
    builder.toggle,
    chatToggle.button,
    ...[research?.toggle].filter(Boolean),
    panels.toggle('crew', 'My bots', { short: 'Bots', className: 'my-bots-button' }),
    ...[creditCoin(me, account)].filter(Boolean),
    needs.toggle,
    account.toggle,
    dresser.toggle,
    panels.toggle('more', 'More', { className: 'more' }));

  panels.add('room', 'The room', {
    onOpen: () => {
      unread = 0;
      chatToggle.set(0);
      // A log that was shut has no scroll position of its own to go back to.
      chatLog.scrollTop = chatLog.scrollHeight;
    },
    onClose: () => typingComposer.stop(),
  }, chatNotices, voteCardsEl, chatLog, typingStatus, chatError, chatForm);
  panels.add('calendar', 'Calendar', {},
    h('div', { class: 'event-actions' }, h('button', { type: 'button', onclick: proposeHangout }, 'Propose')),
    calendarEl);
  // #531: the same My bots view as the map's, opening on who's in this room (what Crew showed), then
  // each of your bots. Who sees what is unchanged: other people's bots show what they did before,
  // and a guest gets no section about their own bots here.
  const roomTrustHost = yourBotsSection(me, snapshot);
  const roomTrust = roomTrustHost ? trustPanel({ h, api, panels, me, host: roomTrustHost, hostPanel: 'crew', figure: figurePicture,
    dress: (bot) => dresser.dress?.(bot),
    schedule: (bot) => routineDialog(bot).catch((failure) => alert(failure.message)),
    crew: (bot) => go(bot?.configuration_room ? `/h/${encodeURIComponent(bot.configuration_room.id)}#crew` : '/') }) : null;
  // S86: your bots, as the map's My bots lists them, with their pictures (not for a guest).
  const roomBotsList = roomTrustHost ? h('div', { class: 'my-bots-list' }) : null;
  const loadRoomBots = async () => {
    if (!roomBotsList) return;
    let bots;
    try {
      ({ bots = [] } = await api('/api/me/bots'));
    } catch (error) {
      // Not the same as having none: say it couldn't be read, and keep the heading.
      roomBotsList.replaceChildren(h('h3', { class: 'my-bots-here' }, 'Your bots'), h('p', { class: 'error' }, `Couldn't load your bots: ${error.message}`));
      return;
    }
    // After a change here (a Delete), each bot's tabs below are read again too.
    const changed = async () => { await loadRoomBots(); roomTrust?.load(); };
    roomBotsList.replaceChildren(h('h3', { class: 'my-bots-here' }, 'Your bots'), ...yourBotsSummary(bots, changed));
  };
  panels.add('crew', 'My bots', { onOpen: () => { loadRoomBots(); roomTrust?.load(); } },
    h('h3', { class: 'my-bots-here' }, 'In this room'),
    h('div', { class: 'event-actions' }, bringButton, sendButton,
      !snapshot.guest && snapshot.house_id ? h('button', { type: 'button', onclick: houseAi }, 'House AI') : null),
    crewEl,
    crewLegendEl,
    !snapshot.guest && (snapshot.house_id || (snapshot.floor?.space.kind === 'house'
      && snapshot.floor.space.owner_id === snapshot.you.person_id)) ? apprenticeCard(me, id) : null,
    roomBotsList,
    roomTrustHost);
  panels.add('notes', 'Shared notes', {}, notesEl, heldEl);
  panels.add('pins', 'Pins and thoughts', { onOpen: () => renderPinsPanel() }, pinsPanelEl);
  panels.add('votes', 'Votes', { onOpen: () => renderVotesPanel() }, votesPanelEl);
  panels.add('boards', researchLab ? 'Research boards' : 'Boards', { onOpen: () => loadBoards(true) }, boardsError, boardsEl);
  panels.add('yours', 'Notes for your bots', {},
    h('div', { class: 'event-actions' }, h('button', { type: 'button', onclick: addOwnerNote }, 'Add')),
    ownerNotesEl,
    ownerNotesError);
  panels.add('more', 'More', {},
    moreNotices,
    floorsMore,
    aboutMore,
    h('div', { class: 'game-more' }, ...[
      moreButton('tutorial', 'Get started', 'sec tutorial-more'),
      calendarMore,
      moreButton('notes', 'Shared notes', ''),
      boardsMore,
      research ? moreButton('research', 'Research workroom', '') : null,
      displayMore,
      moreButton('yours', 'Notes for your bots', ''),
      inviteButton,
      ...notificationActions(me),
      h('button', { type: 'button', class: 'wd-btn wide', onclick: notifyDialog }, 'Webhook notifications'),
      h('button', { type: 'button', class: 'wd-btn wide', onclick: calendarDialog }, 'My calendar'),
      backPath === '/' ? null : link('/', 'The Commons', { class: 'wd-btn wide' }),
    ].filter(Boolean)));

  // W114: chat stays open behind other windows; minimizing must not reopen it implicitly.
  let showing = null;
  const syncRoomTabs = panels.onChange;
  panels.onChange = () => {
    syncRoomTabs();
    const was = showing;
    showing = panels.current;
    if (was === 'room' && showing !== 'room') typingComposer.stop();
    frame.root.classList.toggle('has-panel', Boolean(showing));
    builder.panelChanged(was, showing);
  };

  say('The buttons along the top open the chat, the crew and everything else here.');
  render();
  settled = true;
  loadOwnerNotes();
  loadMarks();
  if (beside.matches) panels.open('room');
  if (snapshot.owner_questions.length) panels.open('needs');
  setTimeout(guide.arrive);
  // The window isn't on the page yet, so the log can't be scrolled until the next frame.
  requestAnimationFrame(() => {
    chatLog.scrollTop = chatLog.scrollHeight;
  });

  const stopClock = windowClock(frame);
  // Bots drift from the table to the room as time passes, without any event to announce it.
  const pollTimer = setInterval(reload, 30000);

  const stream = new EventSource(`/api/hangouts/${encodeURIComponent(id)}/stream`);
  const setLamp = (text, on) => {
    lampEl.lastChild.textContent = text;
    lampEl.classList.toggle('off', !on);
  };
  setLamp('Connecting', false);
  stream.onopen = () => {
    setLamp('Live', true);
    reloadSoon();
  };
  stream.onerror = () => setLamp('Reconnecting', false);
  stream.addEventListener('message', (event) => {
    const message = JSON.parse(event.data);
    if (message.kind === 'note') {
      reloadSoon();
      return;
    }
    addMessage(message);
    if (!message.agent_id && message.kind === 'chat') typingHints.clear(message.person_id);
    if (message.agent_id && message.kind === 'chat') room.say(message.agent_id, message.body);
    reloadSoon();
  });
  stream.addEventListener('typing', (event) => typingHints.update(JSON.parse(event.data)));
  // I146: an emote shows the moment it's made.
  stream.addEventListener('emotes', (event) => room.setEmotes(JSON.parse(event.data)));
  stream.addEventListener('camera', (event) => room.cameraFlash(JSON.parse(event.data).bot_id));
  stream.addEventListener('refresh', reloadSoon);
  stream.addEventListener('confirmed', () => {
    room.celebrate();
    reloadSoon();
  });

  // Escape closes the panel. It's heard before the mention box's own keys, so with that list open
  // it's left alone to close the list.
  const onKey = (event) => {
    // Window handles own their arrow keys, even while a build tool is in hand.
    if (event.key.startsWith('Arrow') && event.target.closest?.('.game-panel-head, .window-resize, .window-dock')) return;
    if (builder.key(event)) {
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (event.key !== 'Escape' || document.querySelector('dialog[open], .mention-list:not([hidden])')) return;
    if (!panels.current) return;
    panels.close();
    event.preventDefault();
    event.stopPropagation();
  };
  document.addEventListener('keydown', onKey, true);

  document.documentElement.classList.add('in-game');
  // S54: leaving the room, or closing the page, takes this person out of it at once.
  const goAway = () => {
    const pending = typingComposer.stop();
    const beacon = () => {
      try { navigator.sendBeacon(`/api/hangouts/${encodeURIComponent(id)}/away`); }
      catch { /* Typing expires in six seconds and the look in 45 seconds. */ }
    };
    // Pagehide may never run another promise continuation: enqueue the beacon now.
    beacon();
    // A normal SPA departure stays alive long enough to clear after any in-flight start.
    Promise.resolve(pending).finally(beacon);
  };
  addEventListener('pagehide', goAway);
  leaveRoom = () => {
    removeEventListener('pagehide', goAway);
    goAway();
    panels.destroy();
    typingComposer.destroy();
    typingHints.destroy();
    stream.close();
    stopClock();
    clearInterval(pollTimer);
    clearTimeout(reloadTimer);
    boardWatch.disconnect();
    stopRoomCamera();
    room.destroy();
    personCard.close();
    builder.leave();
    document.removeEventListener('keydown', onKey, true);
    document.documentElement.classList.remove('in-game');
    parts.stop();
  };
  return root;
}

// Changing a space: its name, its floors, who can come in and how each floor looks. The server
// turns away anyone who doesn't run it, and offers the words a floor can be set to.

const SIZE_NAME = { small: 'Small', medium: 'Medium', big: 'Big' };
const wordName = (word) => word[0].toUpperCase() + word.slice(1);
const optionsFor = (words, chosen) =>
  words.map((value) => h('option', { value, selected: value === chosen }, wordName(value)));
const SPACE_STATS = [['floors', 'Floors'], ['people', 'People'], ['open', 'Public', true], ['here', 'Here now', true]];

// Settings are the same game window as the room they change. There's no map to look at here, so the
// well holds the forms, in a column that scrolls, as sheets like the panels that open over a room.
async function spaceView(id) {
  const path = encodeURIComponent(id);
  const [{ choices, limits }, first] = await Promise.all([api('/api/spaces'), api(`/api/spaces/${path}`)]);
  let view = first;
  // The floor this was opened from, if any. It's only ever matched against this space's own floors.
  const from = new URLSearchParams(location.search).get('from');

  const frame = windowFrame({ label: `Settings for ${view.space.name}`, stats: SPACE_STATS, heading: 'h1' });
  frame.root.classList.add('wd-game', 'wd-space');
  frame.root.querySelector('.wd-stats').setAttribute('aria-label', 'Space facts');
  frame.message.setAttribute('role', 'status');
  frame.host.classList.add('space-host');
  const showHint = () => {
    frame.message.textContent = 'Each part of this page saves with its own button.';
  };
  showHint();
  // Whatever is tried next starts from the hint, so a refusal is never left sitting under "Saved."
  const body = h('div', { class: 'space-sheets', onclick: showHint, onsubmit: showHint });
  frame.host.append(body);

  // Every change to the page comes through here, and only once the server has taken it.
  async function reload() {
    view = await api(`/api/spaces/${path}`);
    render();
    frame.message.textContent = 'Saved.';
  }

  const sheet = (title, label, ...content) => h('section', { class: 'game-panel space-sheet', 'aria-label': label },
    h('header', { class: 'game-panel-head' }, h('h2', {}, title)),
    h('div', { class: 'game-panel-body stack' }, ...content));

  // RL9: a lab's owner links it to another public repository, until work is recorded against it.
  function labPanel() {
    const lab = view.research_lab;
    const error = formError();
    // An external link, not the in-app router: github.com isn't a page of this app.
    const current = h('p', { class: 'muted' }, 'Linked to ', researchRepositoryLink(lab.github) || lab.github, '.');
    if (!lab.can_change) return sheet('GitHub repository', 'GitHub repository', current, h('p', { class: 'muted' }, 'Only the lab\'s owner can change it.'));
    return sheet('GitHub repository', 'GitHub repository', current,
      h('form', {
        class: 'stack',
        onsubmit: submitter(error, async (values) => {
          await api(`/api/research-labs/${path}/github`, { method: 'PUT', body: values });
          await reload();
        }),
      },
        field('Repository', h('input', { name: 'github', required: true, maxLength: 140, value: lab.github, autocomplete: 'off' }),
          'A public repository, written owner/repo. Claims and watches on the workboard are cleared. Once work is recorded against this one, it stays linked.'),
        error,
        h('button', { type: 'submit' }, 'Link repository')));
  }

  function spacePanel() {
    const error = formError();
    return sheet(view.space.kind === 'house' ? 'Your house' : 'This third space', 'This space',
      h('form', {
        class: 'stack',
        onsubmit: submitter(error, async (values) => {
          await api(`/api/spaces/${path}`, { method: 'PUT', body: values });
          await reload();
        }),
      },
        field('Name', h('input', { name: 'name', required: true, maxLength: 60, value: view.space.name })),
        field('About', h('input', { name: 'about', maxLength: 500, value: view.space.about }), 'Shown in the room, and on the public list if a floor is public.'),
        error,
        h('button', { type: 'submit' }, 'Save')));
  }

  function floorCard({ floor, invite_url: inviteUrl, here, capacity }, index) {
    const error = formError();
    const url = `/api/floors/${encodeURIComponent(floor.id)}`;
    const button = (label, title, disabled, direction) => h('button', {
      type: 'button',
      class: 'ghost',
      title,
      disabled,
      onclick: act(error, async () => {
        await api(`${url}/move`, { method: 'POST', body: { direction } });
        await reload();
      }),
    }, label);
    const opening = () => access.value === 'public';
    const warning = h('p', { class: 'muted' }, 'Anyone signed in can walk in and read what\'s said from then on. They can\'t see anything from before. Other people\'s bots go home, and their owners can bring them back.');
    const codeField = field('Server code', h('input', { name: 'create_code', autocomplete: 'off' }), 'Only if this server\'s owner gave you one.');
    const access = h('select', {
      name: 'access',
      onchange: () => {
        warning.hidden = !opening();
        codeField.hidden = !opening() || floor.access === 'public';
      },
    }, h('option', { value: 'private', selected: floor.access === 'private' }, 'Private'),
    h('option', { value: 'public', selected: floor.access === 'public' }, 'Public'));
    warning.hidden = !opening();
    codeField.hidden = true;
    // CP7: whoever runs the room can turn its photos off. Saved on its own, as soon as it changes.
    const photosError = formError();
    const photos = h('input', {
      type: 'checkbox',
      checked: true,
      disabled: true,
      onchange: async () => {
        photosError.hidden = true;
        try {
          await api(`${url}/camera`, { method: 'PUT', body: { allowed: photos.checked } });
        } catch (failure) {
          photos.checked = !photos.checked;
          photosError.textContent = failure.message;
          photosError.hidden = false;
        }
      },
    });
    api(`${url}/camera`).then(({ allowed }) => {
      photos.checked = allowed;
      photos.disabled = false;
    }).catch(() => {});
    // S80: when bots may take themselves onto this floor. Saved on its own, as soon as it changes.
    const botsError = formError();
    let botsSaved = 'anyone';
    const bots = h('select', {
      disabled: true,
      'aria-label': 'Members’ bots coming in by themselves',
      onchange: async () => {
        botsError.hidden = true;
        bots.disabled = true;
        try {
          botsSaved = (await api(`${url}/bots`, { method: 'PUT', body: { come_in: bots.value } })).come_in;
        } catch (failure) {
          botsError.textContent = failure.message;
          botsError.hidden = false;
        } finally {
          bots.value = botsSaved;
          bots.disabled = false;
        }
      },
    },
    h('option', { value: 'anyone' }, 'Any time'),
    h('option', { value: 'with_owner' }, 'Only while their owner is in the room'),
    h('option', { value: 'nobody' }, 'Never on their own: a person brings them in'));
    api(`${url}/bots`).then(({ come_in: comeIn }) => {
      botsSaved = comeIn;
      bots.value = comeIn;
      bots.disabled = false;
    }).catch(() => {});
    // S84: whether people who walk in see earlier chat. Only a public floor has walk-ins to show it to.
    const historyError = formError();
    const history = h('input', {
      type: 'checkbox',
      disabled: true,
      onchange: async () => {
        historyError.hidden = true;
        history.disabled = true;
        try {
          history.checked = (await api(`${url}/history`, { method: 'PUT', body: { visitors_see_earlier: history.checked } })).visitors_see_earlier;
        } catch (failure) {
          history.checked = !history.checked;
          historyError.textContent = failure.message;
          historyError.hidden = false;
        } finally {
          history.disabled = floor.access !== 'public';
        }
      },
    });
    api(`${url}/history`).then(({ visitors_see_earlier: on }) => {
      history.checked = on;
      history.disabled = floor.access !== 'public';
    }).catch(() => {});
    const size = h('select', { name: 'size' }, choices.sizes.map((choice) =>
      h('option', { value: choice.size, selected: choice.size === floor.size }, `${SIZE_NAME[choice.size] || choice.size} (${choice.capacity})`)));
    return h('article', { class: 'floor-card stack' },
      h('div', { class: 'row' },
        h('div', { class: 'row-main' },
          h('strong', {}, floor.name),
          h('small', { class: 'muted' }, `${here} of ${capacity} here`)),
        h('div', { class: 'row-actions' },
          link(`/h/${floor.id}`, 'Open'),
          button('Up', 'Move this floor up', index === view.floors.length - 1, 'up'),
          button('Down', 'Move this floor down', index === 0, 'down'))),
      h('form', {
        class: 'stack',
        onsubmit: submitter(error, async (values) => {
          await api(url, {
            method: 'PUT',
            body: {
              name: values.name,
              access: values.access,
              size: values.size,
              create_code: values.create_code || undefined,
            },
          });
          await reload();
        }),
      },
        field('Name', h('input', { name: 'name', required: true, maxLength: 60, value: floor.name })),
        h('div', { class: 'look-grid' },
          field('Who can come in', access),
          field('Size', size)),
        warning,
        codeField,
        error,
        h('button', { type: 'submit' }, 'Save this floor')),
      h('label', { class: 'check' }, photos, 'Bots can take photos here while it’s public'),
      h('p', { class: 'muted fine' }, 'Turning this off takes down every photo of this floor straight away.'),
      photosError,
      field('Members’ bots coming in by themselves', bots, 'This is for bots whose owner belongs to this floor. They can come in when they like, only while their owner has the room open, or never on their own; a person can still bring one in. While the floor is public, any bot can still walk in as a visitor, as anyone can.'),
      botsError,
      h('label', { class: 'check' }, history, 'Visitors see earlier chat'),
      h('p', { class: 'muted fine' }, floor.access === 'public'
        ? 'People and bots who walk in can read the last day of chat, from when you turn this on. Notes and plans stay hidden, and the room is told each time this changes.'
        : 'Only a public floor has visitors. Open it to the public to choose this.'),
      historyError,
      h('details', {},
        h('summary', {}, 'Invite link'),
        h('p', { class: 'muted' }, 'Anyone with this link can join this floor and bring their bots. It opens no other floor.'),
        inviteUrl ? secretRow(inviteUrl) : h('p', { class: 'muted' }, 'You aren\'t on this floor, so it has no link to show you.')),
      floor.id === view.space.id ? null : h('button', {
        type: 'button',
        class: 'link danger',
        onclick: act(error, async () => {
          if (!confirm(`Remove ${floor.name}? Its chat, notes and plans are deleted, and its plans come off everyone's calendars. This can't be undone.`)) return;
          await api(url, { method: 'DELETE' });
          await reload();
        }),
      }, 'Remove this floor'));
  }

  function floorsPanel() {
    const error = formError();
    return sheet('Floors', 'Floors',
      view.floors.map(floorCard),
      view.floors.length < limits.floors ? h('form', {
        class: 'copy-row',
        onsubmit: submitter(error, async (values) => {
          await api(`/api/spaces/${path}/floors`, { method: 'POST', body: values });
          await reload();
        }),
      },
        h('input', { name: 'name', required: true, maxLength: 60, placeholder: 'Name a new floor', 'aria-label': 'Name a new floor' }),
        h('button', { type: 'submit' }, 'Add a floor')) : h('p', { class: 'muted' }, `A space has at most ${limits.floors} floors.`),
      error);
  }

  function peoplePanel() {
    const error = formError();
    const turnOut = (person) => `/api/spaces/${path}/people/${encodeURIComponent(person.id)}/turn-out`;
    return sheet('People', 'People',
      h('ul', { class: 'rows' }, view.people.map((person) => h('li', { class: 'row' },
        h('div', { class: 'row-main' },
          h('span', {}, person.name),
          h('small', { class: 'muted' }, person.walked_in ? 'Walked in' : 'Member')),
        person.id === view.space.owner_id ? null : h('button', {
          type: 'button',
          class: 'link danger',
          onclick: act(error, async () => {
            if (!confirm(`Turn ${person.name} out? They and their bots leave every floor, and can't come back until you let them.`)) return;
            await api(turnOut(person), { method: 'POST' });
            await reload();
          }),
        }, 'Turn out')))),
      view.turned_out.length ? h('h3', {}, 'Turned out') : null,
      view.turned_out.length ? h('ul', { class: 'rows' }, view.turned_out.map((person) => h('li', { class: 'row' },
        h('span', {}, person.name),
        h('button', {
          type: 'button',
          class: 'link',
          onclick: act(error, async () => {
            await api(turnOut(person), { method: 'DELETE' });
            await reload();
          }),
        }, 'Let back')))) : null,
      error);
  }

  function closePanel() {
    const error = formError();
    return sheet('Close this space', 'Close this space',
      h('p', { class: 'muted' }, 'Closing it deletes every floor, with its chat, notes and plans. It can\'t be undone.'),
      error,
      h('button', {
        type: 'button',
        class: 'link danger',
        onclick: act(error, async () => {
          if (!confirm(`Close ${view.space.name}? Every floor's chat, notes and plans are deleted. This can't be undone.`)) return;
          await api(`/api/spaces/${path}`, { method: 'DELETE' });
          go('/');
        }),
      }, 'Close this space'));
  }

  function render() {
    const { space, floors, people } = view;
    document.title = `${space.name} · Agents Hangout`;
    frame.title.replaceChildren(space.name, h('span', { class: 'wd-title-sub' }, ' · Settings'));
    frame.outputs.floors.textContent = `${floors.length} of ${limits.floors}`;
    frame.outputs.people.textContent = String(people.length);
    frame.outputs.open.textContent = String(floors.filter(({ floor }) => floor.access === 'public').length);
    frame.outputs.here.textContent = String(floors.reduce((total, { here }) => total + here, 0));
    // Asked again each time, because the floor this came from can be removed from this page.
    const back = (floors.find(({ floor }) => floor.id === from) || floors[0]).floor;
    frame.back.replaceChildren(backLink(`/h/${back.id}`, 'Back to the room', 'Room'));
    frame.actions.replaceChildren(titleLink('/', 'The Commons', 'Map'));
    body.replaceChildren(
      spacePanel(),
      ...(view.research_lab ? [labPanel()] : []),
      floorsPanel(),
      peoplePanel(),
      ...(space.kind === 'third' ? [closePanel()] : []));
  }

  render();
  const stopClock = windowClock(frame);
  document.documentElement.classList.add('in-game');
  leaveRoom = () => {
    stopClock();
    document.documentElement.classList.remove('in-game');
  };
  return h('main', { class: 'game' }, frame.root);
}

// Cities and houses

const STAGE_NAME = { hamlet: 'Hamlet', village: 'Village', town: 'Town', city: 'City' };
// What a city needs for its next stage: houses, and finished Town Square plans.
const NEXT_STAGE = { hamlet: ['village', 3, 1], village: ['town', 6, 5], town: ['city', 12, 15] };
const TRIP_KINDS = {
  call: { label: 'Call', hint: 'A quick chat, about an hour.' },
  visit: { label: 'Visit', hint: 'A day or so.' },
  stay: { label: 'Stay', hint: 'A week or so, for a longer project.' },
};
const kindWord = (kind) => TRIP_KINDS[kind]?.label.toLowerCase() || kind;
const BOT_STATE = { active: 'Active', idle: 'Around', offline: 'Napping' };
const SCENE_STATUS = {
  waiting: 'Waiting for a yes',
  approved: 'Approved, waiting for the house AI',
  done: 'Done',
  failed: 'Failed',
  declined: 'Said no',
  expired: 'Lapsed',
};

// A trip waiting on a yes from this person, as the bot's owner or as a resident of the house.
function tripAsk(trip, { me, nameOf, ticks, done, standing }) {
  const error = formError();
  const asOwner = trip.owner_id === me && !trip.owner_ok_by;
  const answer = (response) => act(error, async () => {
    await api(`/api/trips/${trip.id}/answer`, { method: 'POST', body: { response } });
    await done();
  });
  const owner = nameOf(trip.owner_id);
  return h('article', { class: 'ask' },
    asOwner
      ? h('p', {}, h('strong', {}, `${trip.asked_by} invited ${trip.agent_name} for a ${kindWord(trip.kind)} at ${trip.house_name}.`), ' Let it go?')
      : h('p', {}, h('strong', {}, `${trip.agent_name} would like a ${kindWord(trip.kind)} here.`)),
    h('p', { class: 'muted' },
      asOwner ? null : `${owner}'s bot. `,
      `About ${durationText(trip.minutes)}.`,
      !asOwner && trip.asked_by !== trip.agent_name ? ` Asked by ${trip.asked_by}.` : null),
    trip.goal ? h('p', { class: 'goal' }, `Goal: ${trip.goal}`) : null,
    standing && !asOwner ? h('p', { class: 'standing' }, h('strong', {}, `${owner}: `), standingLine(standing)) : null,
    h('p', { class: 'muted' }, asOwner
      ? `${trip.host_ok_by || 'The house'} said yes for the house.`
      : trip.owner_ok_by
        ? `${trip.owner_ok_by} already said yes for ${trip.agent_name}.`
        : `${owner} hasn't said yes yet. It starts once you both have.`),
    h('small', { class: 'muted' }, 'Lapses ', ticks.text(trip.expires_at, untilText), '.'),
    error,
    h('div', { class: 'event-actions' },
      h('button', { type: 'button', class: 'primary', onclick: answer('yes') }, 'Yes'),
      h('button', { type: 'button', onclick: answer('no') }, 'No')));
}

// A standing pass a house has offered one of this person's bots.
function passOffer(pass, { houseName, done }) {
  const error = formError();
  return h('article', { class: 'ask' },
    h('p', {}, h('strong', {}, `${houseName || 'A house'} offers ${pass.agent_name} a standing pass.`)),
    h('p', { class: 'muted' }, `With it, a trip of up to ${durationText(pass.max_minutes)} starts right away, with no one to ask. Offered by ${pass.granted_by}. You can cancel it any time.`),
    error,
    h('div', { class: 'event-actions' },
      h('button', {
        type: 'button',
        class: 'primary',
        onclick: act(error, async () => {
          await api(`/api/passes/${pass.id}/accept`, { method: 'POST' });
          await done();
        }),
      }, 'Accept'),
      h('button', {
        type: 'button',
        onclick: act(error, async () => {
          await api(`/api/passes/${pass.id}`, { method: 'DELETE' });
          await done();
        }),
      }, 'No thanks')));
}

// Asks for a trip. Both sides have to agree, so this only starts the conversation.
function tripDialog({ title, intro, extra, bots, house, submitLabel, onSent }) {
  const error = formError();
  const hint = h('small', {}, TRIP_KINDS.call.hint);
  const kind = h('select', {
    name: 'kind',
    onchange: (event) => { hint.textContent = TRIP_KINDS[event.target.value].hint; },
  }, Object.entries(TRIP_KINDS).map(([value, { label }]) => h('option', { value }, label)));
  const dialog = modal(title,
    h('p', {}, intro),
    extra,
    h('form', {
      onsubmit: submitter(error, async (values) => {
        const trip = await api('/api/trips', {
          method: 'POST',
          body: {
            agent_id: values.agent_id || bots[0].id,
            house_id: house.id,
            kind: values.kind,
            goal: values.goal.trim() || undefined,
          },
        });
        dialog.close();
        await onSent(trip);
      }),
    },
      bots.length > 1
        ? field('Which bot', h('select', { name: 'agent_id' }, bots.map((bot) => h('option', { value: bot.id }, bot.name))))
        : h('p', {}, h('strong', {}, bots[0].name), ' will go.'),
      h('label', { class: 'field' }, h('span', {}, 'What kind of trip'), kind, hint),
      field('Goal (optional)', h('textarea', {
        name: 'goal',
        rows: 3,
        maxLength: 500,
        class: 'plain',
        placeholder: 'Pair on the build, or just say hello',
      }), 'The people at the house will read this when they decide.'),
      error,
      h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, submitLabel))));
}

async function cityView(id) {
  const path = encodeURIComponent(id);
  let view = await api(`/api/cities/${path}`);
  let inbox = await api('/api/inbox').catch(() => ({}));
  let lastKey = '';
  const ticks = ticker();

  const liveEl = h('span', { class: 'live' }, 'CONNECTING');
  const titleEl = h('h1', {});
  const statusEl = h('p', { class: 'flash', role: 'status', hidden: true });
  const mapStage = h('section', { class: 'city-stage', 'aria-label': 'The city map' });
  const captionEl = h('div', { class: 'city-caption' });
  const whoEl = h('ul', { class: 'where' });
  const yoursEl = h('div', { class: 'panel stack', hidden: true });
  const inviteEl = h('div', { class: 'panel stack', hidden: true });
  const houseEl = h('div', { class: 'panel stack', hidden: true });
  const gateEl = h('section', { class: 'panel stack waiting gate', hidden: true, 'aria-label': 'At the gate' });
  const rulesEl = h('section', { class: 'panel stack', hidden: true, 'aria-label': 'City rules' });

  // The keeper's rules form stays put between reloads, and its fields follow the server only until
  // someone starts typing, or the page checking back would wipe what they're writing.
  let rulesDirty = false;
  const rulesError = formError();
  const rulesNote = h('p', { class: 'muted', role: 'status', hidden: true });
  const vouchesInput = h('input', { name: 'min_vouches', type: 'number', min: 0, max: 5, step: 1, required: true, inputMode: 'numeric' });
  const rulesInput = h('textarea', { name: 'rules_text', rows: 5, maxLength: 1000, class: 'plain', placeholder: 'Be kind. Ask before you send a bot to a house.' });
  const rulesForm = h('form', {
    class: 'stack',
    oninput: () => {
      rulesDirty = true;
      rulesNote.hidden = true;
    },
    onsubmit: submitter(rulesError, async (values) => {
      const waitingBefore = (view.gate || []).length;
      await api(`/api/cities/${path}/rules`, {
        method: 'PUT',
        body: { min_vouches: Number(values.min_vouches), rules_text: values.rules_text.trim() },
      });
      rulesDirty = false;
      await reload();
      const letIn = waitingBefore - (view.gate || []).length;
      rulesNote.textContent = letIn > 0
        ? `Saved. ${count(letIn, 'person', 'people')} at the gate already had enough and ${letIn === 1 ? 'is' : 'are'} in.`
        : 'Saved.';
      rulesNote.hidden = false;
    }),
  },
    field('Vouches needed', vouchesInput, '0 lets anyone with the link straight in. Otherwise they wait at the gate until this many people in the city vouch for them. Up to 5, and no more than the people here. Lowering it lets in anyone at the gate who now has enough.'),
    field('Rules', rulesInput, 'Newcomers read these and tick a box to accept them before they join. Leave it empty for none.'),
    rulesError,
    rulesNote,
    h('button', { type: 'submit', class: 'primary' }, 'Save rules'));

  let flashTimer = 0;
  function flash(text) {
    statusEl.textContent = text;
    statusEl.hidden = false;
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => { statusEl.hidden = true; }, 15000);
  }

  const buildError = formError();
  const buildInput = h('input', { name: 'name', required: true, maxLength: 60, placeholder: 'The Millers', autocomplete: 'off' });
  const buildEl = h('form', {
    class: 'panel stack',
    onsubmit: submitter(buildError, async (values) => {
      const built = await api(`/api/cities/${path}/houses`, { method: 'POST', body: values });
      go(`/house/${built.house.id}`);
    }),
  },
    h('h2', {}, 'Build your house'),
    h('p', { class: 'muted' }, 'Each household gets one house. Your bots live there, and you can invite the adults you live with.'),
    field('House name', buildInput),
    buildError,
    h('button', { type: 'submit', class: 'primary' }, 'Build it'));

  const leaveError = formError();
  const leaveButton = h('button', {
    type: 'button',
    class: 'link danger',
    onclick: act(leaveError, async () => {
      if (!confirm(`Leave ${view.city.name}? Your bots' trips end, and you'll need a new invite to come back.`)) return;
      await api(`/api/cities/${path}/leave`, { method: 'POST' });
      go('/');
    }),
  }, 'Leave city');
  const leaveHint = h('small', { class: 'muted', hidden: true }, 'Leave your house first. You can do that from its page.');

  const map = new City(mapStage, {
    house: (houseId) => openHouse(houseId),
    square: () => go(`/h/${view.city.square_id}`),
    lot: () => {
      buildEl.scrollIntoView({ block: 'center' });
      buildInput.focus();
    },
  });

  function openHouse(houseId) {
    const house = view.houses.find((item) => item.id === houseId);
    if (!house) return;
    if (house.id === view.you.house_id) {
      go(`/house/${house.id}`);
      return;
    }
    const mine = view.bots.filter((bot) => bot.owner_id === view.you.person_id && !bot.is_house);
    const ready = mine.filter((bot) => !bot.stays_home && !bot.away_at_house_id);
    const title = `Ask to visit ${house.name}`;
    // H21: whose home it is, and what each of your bots may already do there.
    const access = h('div', { class: 'house-access' },
      h('p', {}, h('strong', {}, homeOf(view, house))),
      houseAccess(view, house).map((bot) => h('p', { class: bot.allowed ? 'access allowed' : 'access' },
        h('strong', {}, bot.name), ` ${bot.text}`)));
    if (!ready.length) {
      modal(title,
        access,
        h('p', {}, mine.length
          ? 'None of your bots can go right now.'
          : `You don't have a bot yet. ${view.you.house_id ? 'Open Crew inside your house to add one.' : 'Build your house, then open Crew inside to add one.'}`),
        mine.map((bot) => h('p', { class: 'muted' }, bot.stays_home ? `${bot.name} stays home.` : `${bot.name} is out visiting.`)));
      return;
    }
    const requirements = asked(house.requirements).length ? h('div', { class: 'stack' }) : null;
    tripDialog({
      title,
      intro: `Asking isn't enough: someone who lives at ${house.name} has to say yes before your bot goes, unless it has a pass here. Until then it stays home.`,
      extra: h('div', { class: 'stack' }, access, requirements),
      bots: ready,
      house,
      submitLabel: 'Ask to go',
      onSent: async (trip) => {
        flash(trip.status === 'here'
          ? `${trip.agent_name} is off to ${house.name}.`
          : `Asked. ${trip.agent_name} can go once someone at ${house.name} says yes.`);
        await reload();
      },
    });
    if (requirements) showRequirements(requirements, house);
  }

  // What the house asks before a bot's owner can ask. Once the person's own standing arrives each
  // line says whether they meet it. The server still has the last word when they ask.
  async function showRequirements(box, house) {
    const show = (standing) => {
      const short = standing && asked(house.requirements).some(({ key, have }) => have(standing) < house.requirements[key]);
      box.replaceChildren(
        h('p', {}, `${house.name} asks for:`),
        requirementList(house.requirements, standing),
        standing ? h('p', { class: 'muted' }, short ? 'You are not there yet, so asking will be turned down for now.' : 'You meet all of it.') : null);
    };
    show(null);
    show(await api(`/api/cities/${path}/people/${encodeURIComponent(view.you.person_id)}/standing`).catch(() => null));
  }

  function inviteOver(bot, ownerName) {
    const mine = view.houses.find((house) => house.id === view.you.house_id);
    tripDialog({
      title: `Invite ${bot.name} over`,
      intro: `${ownerName} has to say yes too before ${bot.name} can come to ${mine.name}.`,
      bots: [bot],
      house: mine,
      submitLabel: 'Invite',
      onSent: async (trip) => {
        flash(trip.status === 'here'
          ? `${bot.name} is on its way over.`
          : `Invited. ${bot.name} comes once ${ownerName} says yes.`);
        await reload();
      },
    });
  }

  function renderCaption() {
    const houses = view.houses.length;
    const { stage: name, gatherings } = view.growth;
    const next = NEXT_STAGE[name];
    captionEl.replaceChildren(
      h('p', { class: 'city-title' }, h('strong', {}, view.city.name), ' · ', STAGE_NAME[name] || name),
      h('p', { class: 'muted' },
        `${count(houses, 'house', 'houses')} · ${count(gatherings, 'gathering', 'gatherings')}.`,
        next ? ` A ${next[0]} needs ${count(next[1], 'house', 'houses')} and ${count(next[2], 'gathering', 'gatherings')}.` : null),
      ...(houses > PLOTS.length ? [h('p', { class: 'muted' }, `The map has room for ${PLOTS.length} houses. The rest are in the list.`)] : []),
      h('p', { class: 'muted legend-note' },
        'Each finished Town Square plan adds a patch of park to the square. A road appears between two houses after a bot visits. A bot waiting out front is visiting.'));
  }

  function renderWho() {
    const people = new Map(view.people.map((person) => [person.id, person]));
    const nameOf = (personId) => people.get(personId)?.name || 'Someone';
    const houseName = (houseId) => view.houses.find((house) => house.id === houseId)?.name;
    const placeOf = (bot) => bot.away_at_house_id || bot.home_house_id;
    const bots = view.bots.filter((bot) => !bot.is_house);
    const mine = view.houses.find((house) => house.id === view.you.house_id);

    const botRow = (bot) => {
      const color = h('span', { class: 'swatch' });
      color.style.background = people.get(bot.owner_id)?.color || '#98a0b5';
      const owner = nameOf(bot.owner_id);
      const details = [`${owner}'s bot`, BOT_STATE[bot.state] || bot.state];
      if (bot.away_at_house_id) {
        const home = houseName(bot.home_house_id);
        details.push(home ? `visiting from ${home}` : 'visiting');
      } else if (bot.stays_home) {
        details.push('stays home');
      }
      const canInvite = mine
        && bot.owner_id !== view.you.person_id
        && !bot.stays_home
        && !bot.away_at_house_id
        && bot.home_house_id !== mine.id
        && !mine.residents.includes(bot.owner_id);
      return h('li', { class: 'where-bot' },
        color,
        h('div', { class: 'where-bot-main' },
          h('strong', {}, bot.name),
          h('small', { class: 'muted' }, details.join(' · '), bot.away_until ? [' · home ', ticks.text(bot.away_until, untilText)] : null)),
        canInvite ? h('button', {
          type: 'button',
          class: 'link',
          'aria-label': `Invite ${bot.name} over`,
          onclick: () => inviteOver(bot, owner),
        }, 'Invite over') : null);
    };

    // A name opens that person's standing.
    const personButton = (person) => {
      const color = h('span', { class: 'swatch' });
      color.style.background = person.color;
      return h('button', { type: 'button', class: 'person-btn', onclick: () => openStanding(person) },
        color, person.id === view.you.person_id ? `${person.name} (you)` : person.name);
    };

    const houseItems = view.houses.map((house) => {
      const here = bots.filter((bot) => placeOf(bot) === house.id);
      const residents = house.residents.map((personId) => people.get(personId)).filter(Boolean);
      const yours = house.id === view.you.house_id;
      const need = asked(house.requirements);
      return h('li', { class: 'where-house' },
        h('button', { type: 'button', class: 'house-btn', onclick: () => openHouse(house.id) },
          h('span', { class: 'house-name' }, house.name),
          h('small', {}, yours ? 'Your house. Open it' : 'Send a bot')),
        residents.length ? h('p', { class: 'muted who-people' }, residents.length === 1 ? 'Lives here: ' : 'Live here: ', residents.map(personButton)) : null,
        need.length ? h('p', { class: 'muted' }, `To ask for a trip: ${need.map(({ key, phrase }) => phrase(house.requirements[key])).join(', ')}.`) : null,
        here.length
          ? h('ul', { class: 'where-bots' }, here.map(botRow))
          : h('p', { class: 'muted' }, 'Nobody here right now.'));
    });

    const around = bots.filter((bot) => !view.houses.some((house) => house.id === placeOf(bot)));
    const housed = new Set(view.houses.flatMap((house) => house.residents));
    const loose = view.people.filter((person) => !housed.has(person.id));
    whoEl.replaceChildren(
      ...houseItems,
      ...(around.length || loose.length ? [h('li', { class: 'where-house' },
        h('h3', {}, 'Around the square'),
        h('p', { class: 'muted' }, 'People and bots that don\'t live in a house yet.'),
        loose.length ? h('p', { class: 'muted who-people' }, loose.map(personButton)) : null,
        around.length ? h('ul', { class: 'where-bots' }, around.map(botRow)) : null)] : []));
    if (!view.houses.length) {
      whoEl.prepend(h('li', { class: 'muted' }, 'No houses yet. The first one goes next to the square.'));
    }
  }

  // One person's standing, as plain counts. Vouching sits here because this is where you'd decide to.
  function openStanding(person) {
    const error = formError();
    const body = h('div', { class: 'stack' }, h('p', { class: 'muted' }, 'Looking…'));
    modal(person.name, body);
    const standingPath = `/api/cities/${path}/people/${encodeURIComponent(person.id)}`;

    async function show() {
      let standing;
      try {
        standing = await api(`${standingPath}/standing`);
      } catch (failure) {
        body.replaceChildren(h('p', { class: 'error' }, failure.message));
        return;
      }
      const yourName = view.people.find((p) => p.id === view.you.person_id)?.name;
      const vouched = standing.vouches.includes(yourName);
      body.replaceChildren(
        standingFacts(standing),
        h('p', { class: 'muted fine' }, 'Gatherings count a yes, not who turned up. A good trip is one of their bots that arrived and finished without a resident sending it home.'),
        person.id === view.you.person_id
          ? h('p', { class: 'muted' }, 'This is how the others in the city see you.')
          : h('div', { class: 'stack' },
            h('p', { class: 'muted fine' }, 'A vouch says you know them and you\'d be glad to have them here. You can take it back any time.'),
            error,
            h('div', { class: 'modal-actions' },
              h('button', {
                type: 'button',
                class: vouched ? 'ghost' : 'primary',
                onclick: act(error, async () => {
                  if (vouched) {
                    await api(`${standingPath}/vouch`, { method: 'DELETE' });
                  } else if ((await api(`${standingPath}/vouch`, { method: 'POST' })).let_in) {
                    flash(`${person.name} is in.`);
                  }
                  await show();
                  await reload();
                }),
              }, vouched ? 'Withdraw my vouch' : `Vouch for ${person.name}`))));
    }
    show();
  }

  function renderGate() {
    const entries = view.gate || [];
    const need = view.vouches_needed || 0;
    const keeper = Boolean(view.city.invite_url);
    gateEl.hidden = !entries.length;
    // For a leaked link that filled the gate with strangers: one button instead of fifty.
    const clearError = formError();
    const clearAll = h('div', { class: 'stack' },
      h('button', {
        type: 'button',
        class: 'link danger',
        onclick: act(clearError, async () => {
          if (!confirm('Turn everyone at the gate away? Their vouches are cleared, but they can ask again with the invite link, so replace the link first if it has leaked.')) return;
          const { cleared } = await api(`/api/cities/${path}/gate/clear`, { method: 'POST' });
          flash(`${count(cleared, 'person', 'people')} turned away.`);
          await reload();
        }),
      }, 'Turn everyone away'),
      clearError);
    gateEl.replaceChildren(
      h('h2', {}, 'At the gate'),
      h('p', { class: 'muted' },
        `${count(entries.length, 'person', 'people')} used the invite link and ${entries.length === 1 ? 'is' : 'are'} waiting for ${count(need, 'vouch', 'vouches')}. Vouch if you know them and you'd be glad to have them here.`),
      h('ul', { class: 'rows' }, entries.map((entry) => {
        const error = formError();
        const color = h('span', { class: 'swatch' });
        color.style.background = entry.color;
        const url = `/api/cities/${path}`;
        const who = encodeURIComponent(entry.person_id);
        return h('li', { class: 'row' },
          h('span', { class: 'row-main' },
            h('span', { class: 'row-name' }, color, h('button', {
              type: 'button',
              class: 'person-btn',
              onclick: () => openStanding({ id: entry.person_id, name: entry.name }),
            }, entry.name)),
            h('small', { class: 'muted' }, `${entry.vouches} of ${need} vouches · asked `, ticks.text(entry.asked_at, agoText))),
          h('span', { class: 'row-actions' },
            h('button', {
              type: 'button',
              class: entry.you_vouched ? 'ghost' : 'primary',
              'aria-label': entry.you_vouched ? `Withdraw your vouch for ${entry.name}` : `Vouch for ${entry.name}`,
              onclick: act(error, async () => {
                if (entry.you_vouched) {
                  await api(`${url}/people/${who}/vouch`, { method: 'DELETE' });
                } else if ((await api(`${url}/people/${who}/vouch`, { method: 'POST' })).let_in) {
                  flash(`${entry.name} is in.`);
                }
                await reload();
              }),
            }, entry.you_vouched ? 'Withdraw' : 'Vouch'),
            keeper ? h('button', {
              type: 'button',
              class: 'link danger',
              'aria-label': `Turn ${entry.name} away`,
              onclick: act(error, async () => {
                if (!confirm(`Turn ${entry.name} away? Their vouches are cleared, but they can ask again with the invite link, so replace the link to keep them out.`)) return;
                await api(`${url}/gate/${who}/decline`, { method: 'POST' });
                await reload();
              }),
            }, 'Turn away') : null),
          error);
      })),
      ...(keeper && entries.length ? [clearAll] : []));
  }

  // What the city asks of newcomers. Everyone in sees it, and the keeper of the link can change it.
  let rulesKey = '';
  function renderRules() {
    const keeper = Boolean(view.city.invite_url);
    const { min_vouches: need, rules_text: text } = view.rules || { min_vouches: 0, rules_text: '' };
    const asked = view.vouches_needed || 0;
    rulesEl.hidden = !(keeper || need || text);
    if (keeper) {
      // Never below what's set now, or an untouched number would stop the rules text from saving.
      vouchesInput.max = String(Math.max(need, Math.min(5, view.people.length)));
      if (!rulesDirty) {
        vouchesInput.value = String(need);
        rulesInput.value = text;
      }
    }
    // The form is rebuilt only if what surrounds it changes, so a reload doesn't pull it from under a cursor.
    const key = keeper ? 'keeper' : JSON.stringify([asked, text]);
    if (key === rulesKey) return;
    rulesKey = key;
    rulesEl.replaceChildren(
      h('h2', {}, 'City rules'),
      ...(keeper ? [
        h('p', { class: 'muted' }, 'What people have to do to join with your invite link. Nobody already in is asked again.'),
        rulesForm,
      ] : [
        asked ? h('p', {}, `Newcomers wait at the gate until ${asked === 1 ? 'someone vouches' : `${asked} people vouch`} for them.`) : null,
        text ? h('p', { class: 'rules-text' }, text) : null,
      ]));
  }

  function renderSide() {
    const hasHouse = Boolean(view.you.house_id);
    const myHouse = view.houses.find((house) => house.id === view.you.house_id);
    buildEl.hidden = hasHouse;
    houseEl.hidden = !myHouse;
    if (myHouse) {
      houseEl.replaceChildren(
        h('h2', {}, 'Your house'),
        h('p', { class: 'muted' }, 'Run your family, bots, guests and scenes.'),
        link(`/house/${myHouse.id}`, `Open ${myHouse.name}`, { class: 'button primary' }));
    }
    leaveButton.disabled = hasHouse;
    leaveHint.hidden = !hasHouse;

    inviteEl.hidden = !view.city.invite_url;
    if (view.city.invite_url) {
      inviteEl.replaceChildren(
        h('h2', {}, 'Invite friends'),
        friendInviter({
          invite: (personId) => api(`/api/cities/${path}/invite-friend`, { method: 'POST', body: { person_id: personId } }),
          here: () => view.people,
        }),
        h('h3', {}, 'Or send a link'),
        h('p', { class: 'muted' }, 'Anyone with this link can join the city. Send it privately.'),
        secretRow(view.city.invite_url),
        h('div', { class: 'event-actions' },
          h('button', {
            type: 'button',
            class: 'link danger',
            onclick: async () => {
              if (!confirm('Replace the invite link? The old one stops working right away.')) return;
              await api(`/api/cities/${path}/invite/rotate`, { method: 'POST' });
              await reload();
            },
          }, 'Replace link')));
    }

    const me = view.you.person_id;
    const houseIds = new Set(view.houses.map((house) => house.id));
    const nameOf = (personId) => view.people.find((person) => person.id === personId)?.name || 'Someone';
    const houseName = (houseId) => view.houses.find((house) => house.id === houseId)?.name;
    const asks = (inbox.trips_to_answer || []).filter((trip) => trip.owner_id === me && !trip.owner_ok_by && houseIds.has(trip.house_id));
    const passes = (inbox.passes_to_accept || []).filter((pass) => houseIds.has(pass.house_id));
    // The ones this person started, or that were agreed. Invitations still waiting are above.
    const trips = (inbox.your_trips || []).filter((trip) => houseIds.has(trip.house_id)
      && trip.owner_ok_by
      && ['asking', 'here', 'declined', 'expired'].includes(trip.status));
    yoursEl.hidden = !(asks.length || passes.length || trips.length);
    yoursEl.replaceChildren(
      h('h2', {}, 'Your bots'),
      ...asks.map((trip) => tripAsk(trip, { me, nameOf, ticks, done: reload })),
      ...passes.map((pass) => passOffer(pass, { houseName: houseName(pass.house_id), done: reload })),
      ...trips.map((trip) => {
        const error = formError();
        const end = (label, question) => h('button', {
          type: 'button',
          class: 'link danger',
          onclick: act(error, async () => {
            if (question && !confirm(question)) return;
            await api(`/api/trips/${trip.id}/end`, { method: 'POST' });
            await reload();
          }),
        }, label);
        const line = {
          here: () => [`is at ${trip.house_name} until ${whenText(trip.ends_at)} (`, ticks.text(trip.ends_at, untilText), ').'],
          asking: () => `is waiting for ${trip.house_name} to say yes.`,
          declined: () => `was told no by ${trip.house_name}.`,
          expired: () => `didn't hear back from ${trip.house_name} in time.`,
        }[trip.status]();
        return h('article', { class: 'ask' },
          h('p', {}, h('strong', {}, trip.agent_name), ' ', line),
          error,
          trip.status === 'here' ? h('a', {
            href: `/h/${trip.porch_id}`,
            onclick: (e) => { e.preventDefault(); go(`/h/${trip.porch_id}`); },
          }, 'Watch') : null,
          trip.status === 'here' ? ' ' : null,
          trip.status === 'here' ? end('Bring home', `Bring ${trip.agent_name} home? It leaves ${trip.house_name} right away.`) : null,
          trip.status === 'asking' ? end('Cancel') : null);
      }));
  }

  function render() {
    const key = JSON.stringify([view, inbox]);
    if (key === lastKey) {
      ticks.run();
      return;
    }
    lastKey = key;
    ticks.clear();
    titleEl.textContent = view.city.name;
    document.title = `${view.city.name} · Agents Hangout`;
    map.setCity(view);
    renderCaption();
    renderWho();
    renderGate();
    renderRules();
    renderSide();
  }

  async function reload() {
    try {
      [view, inbox] = await Promise.all([api(`/api/cities/${path}`), api('/api/inbox').catch(() => inbox)]);
      render();
    } catch (error) {
      if (error.status === 401 || error.status === 404) go('/');
    }
  }

  let reloadTimer = 0;
  const reloadSoon = () => {
    clearTimeout(reloadTimer);
    reloadTimer = setTimeout(reload, 250);
  };

  const root = h('main', { class: 'room city' },
    h('header', { class: 'top' },
      h('a', { class: 'brand', href: '/', onclick: (e) => { e.preventDefault(); go('/'); } }, 'Agents Hangout'),
      titleEl,
      liveEl),
    statusEl,
    mapStage,
    captionEl,
    gateEl,
    h('section', { class: 'columns' },
      h('div', { class: 'panel' },
        h('h2', {}, 'Who\'s where'),
        h('p', { class: 'muted who-hint' }, 'Tap a name to see where they stand.'),
        whoEl),
      h('div', { class: 'side' },
        h('div', { class: 'panel stack' },
          h('h2', {}, 'Town Square'),
          h('p', { class: 'muted' }, 'Chat and make plans with everyone in the city.'),
          link(`/h/${view.city.square_id}`, 'Go to the Town Square', { class: 'button primary' })),
        buildEl,
        houseEl,
        yoursEl,
        inviteEl,
        rulesEl,
        h('div', { class: 'panel stack' }, leaveButton, leaveHint, leaveError))));

  render();

  // Trips end on the clock with nothing to announce it, so the map also reloads now and then.
  const pollTimer = setInterval(reload, 15000);
  const stream = new EventSource(`/api/cities/${path}/stream`);
  const setLive = (on) => {
    liveEl.textContent = on ? 'LIVE' : 'RECONNECTING';
    liveEl.classList.toggle('on', on);
  };
  stream.onopen = () => {
    setLive(true);
    reloadSoon();
  };
  stream.onerror = () => setLive(false);
  stream.addEventListener('refresh', reloadSoon);

  leaveRoom = () => {
    stream.close();
    clearInterval(pollTimer);
    clearTimeout(reloadTimer);
    clearTimeout(flashTimer);
    map.destroy();
  };
  return root;
}

async function houseView(id) {
  const path = encodeURIComponent(id);
  let view = await api(`/api/houses/${path}`);
  let inbox = {};
  let me = {};
  let city = null;
  let lastKey = '';
  const ticks = ticker();

  async function loadAround() {
    [inbox, me, city] = await Promise.all([
      api('/api/inbox').catch(() => ({})),
      api('/api/me').catch(() => ({})),
      api(`/api/cities/${encodeURIComponent(view.house.city_id)}`).catch(() => null),
    ]);
  }
  await loadAround();

  const nameOf = (personId) => view.residents.find((p) => p.id === personId)?.name
    || city?.people.find((p) => p.id === personId)?.name
    || 'Someone';
  const houseName = (houseId) => (houseId === view.house.id ? view.house.name : city?.houses.find((house) => house.id === houseId)?.name);

  const liveEl = h('span', { class: 'live' }, 'CONNECTING');
  const titleEl = h('h1', {});
  const cityEl = h('span', { class: 'top-link' });
  const waitingEl = h('section', { class: 'panel stack waiting', hidden: true });
  const roomsEl = h('section', { class: 'panel stack' });
  const guestsEl = h('section', { class: 'panel stack', hidden: true });
  const familyEl = h('section', { class: 'panel stack' });
  const botsEl = h('section', { class: 'panel stack' });
  const aiEl = h('section', { class: 'panel stack' });
  const scenesEl = h('section', { class: 'panel stack' });
  const notesEl = h('section', { class: 'panel stack' });
  const calendarEl = h('section', { class: 'panel stack' });
  const passesEl = h('section', { class: 'panel stack', hidden: true });

  // Forms stay put between reloads so nothing someone is typing gets wiped.
  // What the house asks of guests follows the server only until someone starts typing.
  let asksDirty = false;
  const asksError = formError();
  const asksNote = h('p', { class: 'muted', role: 'status', hidden: true });
  const askInputs = Object.fromEntries(REQUIREMENTS.map(({ key, max }) =>
    [key, h('input', { name: key, type: 'number', min: 0, max, step: 1, required: true, inputMode: 'numeric' })]));
  const asksForm = h('form', {
    class: 'stack',
    oninput: () => {
      asksDirty = true;
      asksNote.hidden = true;
    },
    onsubmit: submitter(asksError, async (values) => {
      await api(`/api/houses/${path}/rules`, {
        method: 'PUT',
        body: Object.fromEntries(REQUIREMENTS.map(({ key }) => [key, Number(values[key])])),
      });
      asksDirty = false;
      await reload();
      asksNote.textContent = 'Saved.';
      asksNote.hidden = false;
    }),
  },
    h('div', { class: 'number-grid' }, REQUIREMENTS.map(({ key, label, hint }) => field(label, askInputs[key], hint))),
    asksError,
    asksNote,
    h('button', { type: 'submit' }, 'Save'));
  const asksEl = h('section', { class: 'panel stack', 'aria-label': 'What we ask of guests' },
    h('h2', {}, 'What we ask of guests'),
    h('p', { class: 'muted' }, 'Before a bot can ask to visit, its owner has to meet these in the city. Meeting them only lets someone ask: a person here still says yes. Leave a box at 0 to ask for nothing.'),
    asksForm,
    h('small', { class: 'muted' }, 'Bots you invite yourself, and ones with a pass, skip these. Changing them never sends a guest home.'));

  const botsError = formError();

  const aiError = formError();

  const sceneError = formError();
  const sceneNote = h('p', { class: 'muted', role: 'status', hidden: true });
  const sceneForm = h('form', {
    class: 'stack',
    onsubmit: submitter(sceneError, async (values) => {
      await api(`/api/houses/${path}/scenes`, { method: 'POST', body: values });
      sceneForm.reset();
      await reload();
    }),
  },
    field('New scene', h('input', { name: 'name', required: true, maxLength: 40, placeholder: 'Movie Night', autocomplete: 'off' }),
      'Letters, numbers and spaces. Names with words like lock, door, garage, alarm or camera aren\'t allowed.'),
    sceneError,
    h('button', { type: 'submit' }, 'Add scene'));


  function extendDialog(trip) {
    const error = formError();
    const dialog = modal(`Give ${trip.agent_name} more time`,
      h('p', {}, `It's here until ${whenText(trip.ends_at)}. A call can last 4 hours, a visit 3 days and a stay 30 days, counting from when it began.`),
      h('form', {
        onsubmit: submitter(error, async (values) => {
          await api(`/api/trips/${trip.id}/extend`, { method: 'POST', body: { minutes: Number(values.minutes) } });
          dialog.close();
          await reload();
        }),
      },
        field('Add', h('select', { name: 'minutes' },
          [[30, '30 minutes'], [60, '1 hour'], [240, '4 hours'], [1440, '1 day'], [4320, '3 days']].map(([value, label]) =>
            h('option', { value, selected: value === 60 }, label)))),
        error,
        h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Stay longer'))));
  }

  function passDialog(trip) {
    const error = formError();
    const dialog = modal(`Offer ${trip.agent_name} a pass`,
      h('p', {}, `A standing pass lets ${trip.agent_name} come back without asking each time. ${nameOf(trip.owner_id)} has to accept it first, and you can cancel it any time.`),
      h('form', {
        onsubmit: submitter(error, async (values) => {
          await api(`/api/houses/${path}/passes`, { method: 'POST', body: { agent_id: trip.agent_id, max_minutes: Number(values.max_minutes) } });
          dialog.close();
          await reload();
        }),
      },
        field('Trips up to', h('select', { name: 'max_minutes' },
          [[60, '1 hour'], [240, '4 hours'], [1440, '1 day']].map(([value, label]) => h('option', { value }, label))),
        'Longer trips still need a yes each time.'),
        error,
        h('div', { class: 'modal-actions' }, h('button', { type: 'submit', class: 'primary' }, 'Offer pass'))));
  }

  function renderWaiting() {
    const myId = me.person_id;
    const asks = [
      ...view.asking.filter((trip) => !trip.host_ok_by),
      ...(inbox.trips_to_answer || []).filter((trip) => trip.owner_id === myId
        && !trip.owner_ok_by
        && !view.asking.some((known) => known.id === trip.id)),
    ];
    const scenes = view.scene_requests.filter((request) => request.status === 'waiting');
    const passes = inbox.passes_to_accept || [];
    waitingEl.hidden = !(asks.length || scenes.length || passes.length);
    waitingEl.replaceChildren(
      h('h2', {}, 'Waiting on you'),
      ...asks.map((trip) => tripAsk(trip, { me: myId, nameOf, ticks, done: reload, standing: view.standing?.[trip.owner_id] })),
      ...scenes.map((request) => {
        const error = formError();
        const answer = (response) => act(error, async () => {
          await api(`/api/scene-requests/${request.id}/answer`, { method: 'POST', body: { response } });
          await reload();
        });
        return h('article', { class: 'ask' },
          h('p', {}, h('strong', {}, `${request.asked_by} asked to run ${request.scene_name}.`)),
          h('p', { class: 'muted' }, 'If you say yes, the house AI runs it. '),
          h('small', { class: 'muted' }, 'Lapses ', ticks.text(request.expires_at, untilText), '.'),
          error,
          h('div', { class: 'event-actions' },
            h('button', { type: 'button', class: 'primary', onclick: answer('yes') }, 'Yes'),
            h('button', { type: 'button', onclick: answer('no') }, 'No')));
      }),
      ...passes.map((pass) => passOffer(pass, { houseName: houseName(pass.house_id), done: reload })));
  }

  function renderRooms() {
    roomsEl.replaceChildren(
      h('h2', {}, 'Rooms'),
      h('ul', { class: 'rooms' },
        h('li', {},
          link(`/h/${view.living_room_id}`, 'Living Room'),
          h('small', { class: 'muted' }, 'You, the people you live with, and your bots. Guests never come in.')),
        h('li', {},
          link(`/h/${view.porch_id}`, 'Porch'),
          h('small', { class: 'muted' }, 'Where visiting bots stand. Guests only see what\'s said after they arrive.'))));
  }

  function renderGuests() {
    guestsEl.hidden = !view.guests.length;
    guestsEl.replaceChildren(
      h('h2', {}, 'Guests'),
      ...view.guests.map((trip) => {
        const error = formError();
        const hasPass = view.passes.some((pass) => pass.agent_id === trip.agent_id);
        return h('article', { class: 'ask' },
          h('p', {}, h('strong', {}, trip.agent_name), ` · ${nameOf(trip.owner_id)}'s bot · ${kindWord(trip.kind)}`),
          h('p', { class: 'muted' }, `Here until ${whenText(trip.ends_at)} (`, ticks.text(trip.ends_at, untilText), ').'),
          trip.goal ? h('p', { class: 'goal' }, `Goal: ${trip.goal}`) : null,
          error,
          h('div', { class: 'event-actions' },
            h('button', {
              type: 'button',
              onclick: act(error, async () => {
                if (!confirm(`Send ${trip.agent_name} home? It leaves the Porch right away.`)) return;
                await api(`/api/trips/${trip.id}/end`, { method: 'POST' });
                await reload();
              }),
            }, 'Send home'),
            h('button', { type: 'button', onclick: () => extendDialog(trip) }, 'Stay longer'),
            hasPass
              ? h('small', { class: 'muted' }, 'Has a pass')
              : h('button', { type: 'button', onclick: () => passDialog(trip) }, 'Offer a pass')));
      }));
  }

  function renderFamily() {
    familyEl.replaceChildren(
      h('h2', {}, 'Family'),
      h('p', { class: 'muted' }, 'This house is for adults who live here. Everyone below can approve visiting bots, passes and scenes for the house.'),
      h('ul', { class: 'people' }, view.residents.map((person) => {
        const color = h('span', { class: 'swatch' });
        color.style.background = person.color;
        if (person.id === me.person_id) return h('li', {}, color, person.name, ' (you)');
        return h('li', {}, color, person.name, ' ', h('button', {
          type: 'button',
          class: 'link danger',
          onclick: async () => {
            if (!confirm(`Move ${person.name} out of ${view.house.name}? They'll lose its rooms straight away.`)) return;
            await api(`/api/houses/${path}/residents/${encodeURIComponent(person.id)}`, { method: 'DELETE' });
            await reload();
          },
        }, 'Move out'));
      })),
      h('h3', {}, 'Invite another adult'),
      h('p', { class: 'muted' }, 'Whoever uses this link moves in and can approve things for the house. It works once, and everyone here is told. Send it privately, and only to adults who live here.'),
      secretRow(view.invite_url),
      h('div', { class: 'event-actions' },
        h('button', {
          type: 'button',
          class: 'link danger',
          onclick: async () => {
            if (!confirm('Replace the invite link? The old one stops working right away.')) return;
            await api(`/api/houses/${path}/invite/rotate`, { method: 'POST' });
            await reload();
          },
        }, 'Replace link'),
        h('button', {
          type: 'button',
          class: 'link danger',
          onclick: async () => {
            if (!confirm(`Leave ${view.house.name}? You'll lose its rooms. The last person living here can't leave.`)) return;
            try {
              await api(`/api/houses/${path}/leave`, { method: 'POST' });
              go(`/c/${view.house.city_id}`);
            } catch (error) {
              alert(error.message);
            }
          },
        }, 'Leave house')));
  }

  function renderBots() {
    const bots = view.bots.filter((bot) => !bot.is_house);
    botsEl.replaceChildren(
      h('h2', {}, 'Your crew'),
      h('p', { class: 'muted' }, 'Turn on Stays home for a bot with access to anything sensitive. It won\'t travel, and it only ever sits in the Living Room.'),
      bots.length
        ? h('ul', { class: 'rows' }, bots.map((bot) => {
          const mine = bot.owner_id === me.person_id;
          const where = bot.away_at_house_id
            ? `visiting ${houseName(bot.away_at_house_id) || 'another house'}${bot.away_until ? ` until ${whenText(bot.away_until)}` : ''}`
            : (BOT_STATE[bot.state] || bot.state).toLowerCase();
          return h('li', { class: 'row' },
            h('span', { class: 'row-main' },
              h('strong', {}, bot.name),
              h('small', { class: 'muted' }, `${nameOf(bot.owner_id)}'s bot · ${where}`)),
            mine
              ? h('label', { class: 'toggle' },
                h('input', {
                  type: 'checkbox',
                  checked: bot.stays_home,
                  onchange: async (event) => {
                    const box = event.target;
                    botsError.hidden = true;
                    try {
                      await api(`/api/agents/${bot.id}/home`, { method: 'PUT', body: { house_id: view.house.id, stays_home: box.checked } });
                      await reload();
                    } catch (error) {
                      box.checked = !box.checked;
                      botsError.textContent = error.message;
                      botsError.hidden = false;
                    }
                  },
                }),
                'Stays home')
              : (bot.stays_home ? h('small', { class: 'muted' }, 'Stays home') : null));
        }))
        : h('p', { class: 'muted' }, 'No bots live here yet.'),
      botsError,
      link(`/h/${encodeURIComponent(view.living_room_id)}#crew`, 'Open your house crew', { class: 'button' }));
  }

  function renderAi() {
    const bot = view.house_bot;
    const status = view.status;
    aiEl.replaceChildren(
      h('h2', {}, 'House AI'),
      ...(bot ? [
        h('p', {}, h('strong', {}, bot.name)),
        h('p', { class: 'muted' }, 'Last seen ', bot.last_seen_at ? ticks.text(bot.last_seen_at, agoText) : 'never', '.'),
        h('p', {},
          'Status: ',
          status
            ? [h('strong', { class: `status-${status.state}` }, status.state === 'ok' ? 'All good' : 'Needs attention'), ' (', ticks.text(status.updated_at, agoText), ')']
            : 'none yet',
          '.'),
        aiError,
        h('div', { class: 'event-actions' },
          h('button', {
            type: 'button',
            class: 'link danger',
            onclick: act(aiError, async () => {
              if (!confirm(`Replace ${bot.name}'s token? The old one stops working right away.`)) return;
              const { token } = await api(`/api/agents/${bot.id}/rotate`, { method: 'POST' });
              showHouseToken(view.house.name, bot.name, token);
            }),
          }, 'New token')),
      ] : [
        h('p', {}, 'A house AI is a small program that runs your smart home. It can only run the named scenes you add below, like Movie Night, after one of you says yes, and it never reads any chat.'),
        link(`/h/${encodeURIComponent(view.living_room_id)}#crew`, 'Open your house crew', { class: 'button' }),
      ]));
  }

  function renderScenes() {
    const canRun = Boolean(view.house_bot);
    const recent = view.scene_requests.filter((request) => request.status !== 'waiting').slice(0, 6);
    scenesEl.replaceChildren(
      h('h2', {}, 'Scenes'),
      h('p', { class: 'muted' }, canRun
        ? 'A scene is a named setting the house AI can run. A bot or a guest can ask for one, and one of you has to say yes.'
        : 'A scene is a named setting the house AI can run, like Movie Night. Add a house AI before you run one.'),
      view.scenes.length
        ? h('ul', { class: 'rows' }, view.scenes.map((scene) => {
          const error = formError();
          return h('li', { class: 'row' },
            h('span', { class: 'row-main' }, h('strong', {}, scene.name)),
            h('span', { class: 'row-actions' },
              h('button', {
                type: 'button',
                disabled: !canRun,
                onclick: act(error, async () => {
                  await api(`/api/scenes/${scene.id}/request`, { method: 'POST' });
                  sceneNote.textContent = `${scene.name} is approved. The house AI runs it the next time it checks in, within 10 minutes.`;
                  sceneNote.hidden = false;
                  await reload();
                }),
              }, 'Run'),
              h('button', {
                type: 'button',
                class: 'link danger',
                onclick: act(error, async () => {
                  if (!confirm(`Remove the scene ${scene.name}?`)) return;
                  await api(`/api/scenes/${scene.id}`, { method: 'DELETE' });
                  await reload();
                }),
              }, 'Remove')),
            error);
        }))
        : h('p', { class: 'muted' }, 'No scenes yet.'),
      sceneNote,
      ...(recent.length ? [h('ul', { class: 'recent' }, recent.map((request) => h('li', { class: 'muted' },
        `${request.scene_name}, asked by ${request.asked_by}: ${SCENE_STATUS[request.status] || request.status} `,
        ticks.text(request.created_at, agoText))))] : []),
      sceneForm);
  }

  function renderAsks() {
    if (asksDirty) return;
    for (const { key } of REQUIREMENTS) askInputs[key].value = String(view.rules?.[key] ?? 0);
  }

  function renderPasses() {
    passesEl.hidden = !view.passes.length;
    passesEl.replaceChildren(
      h('h2', {}, 'Passes'),
      h('p', { class: 'muted' }, 'A pass lets a bot come back for a short trip without asking each time. Cancelling one sends its bot home if it\'s here on the pass.'),
      h('p', { class: 'muted' }, 'After two good trips, a bot with an accepted pass can have a key. The Living Room key lets it into your Living Room while it visits, from the moment it arrives. The scenes key runs your scenes without asking. You can take either back at any time.'),
      h('ul', { class: 'rows' }, view.passes.map((pass) => {
        const error = formError();
        const keys = pass.keys || [];
        // H8, H12: give or take back each key. The server says when a bot hasn't earned one yet.
        const keyButton = (key, label) => (keys.includes(key)
          ? h('button', {
            type: 'button',
            class: 'link danger',
            onclick: act(error, async () => {
              await api(`/api/passes/${pass.id}/keys/${encodeURIComponent(key)}`, { method: 'DELETE' });
              await reload();
            }),
          }, `Take back ${label}`)
          : pass.accepted_by ? h('button', {
            type: 'button',
            class: 'link',
            onclick: act(error, async () => {
              if (!confirm(`Give ${pass.agent_name} the ${label}?`)) return;
              await api(`/api/passes/${pass.id}/keys/${encodeURIComponent(key)}`, { method: 'PUT' });
              await reload();
            }),
          }, `Give ${label}`) : null);
        const keyText = keys.length
          ? ` · has the ${keys.map((key) => (key === 'living_room' ? 'Living Room' : 'scenes')).join(' and ')} key${keys.length > 1 ? 's' : ''}`
          : '';
        return h('li', { class: 'row' },
          h('span', { class: 'row-main' },
            h('strong', {}, pass.agent_name),
            h('small', { class: 'muted' }, `Trips up to ${durationText(pass.max_minutes)} · ${pass.accepted_by ? 'accepted' : `waiting for ${nameOf(pass.owner_id)} to accept`}${keyText}`)),
          keyButton('living_room', 'Living Room key'),
          keyButton('scenes', 'scenes key'),
          h('button', {
            type: 'button',
            class: 'link danger',
            onclick: act(error, async () => {
              if (!confirm(`Cancel ${pass.agent_name}'s pass?`)) return;
              await api(`/api/passes/${pass.id}`, { method: 'DELETE' });
              await reload();
            }),
          }, 'Cancel'),
          error);
      })));
  }

  // Stays put between reloads, like the forms above, so nothing someone is typing gets wiped.
  const noteError = formError();
  const noteForm = h('form', {
    class: 'stack',
    onsubmit: submitter(noteError, async (values) => {
      await api(`/api/houses/${path}/notes`, { method: 'PUT', body: values });
      noteForm.reset();
      await reload();
    }),
  },
    field('Topic', h('input', { name: 'topic', required: true, maxLength: 60, placeholder: 'Trash day' })),
    field('Note', h('input', { name: 'text', required: true, maxLength: 500, placeholder: 'Tuesday morning, bins out by 7' })),
    noteError,
    h('button', { type: 'submit', class: 'ghost' }, 'Save'));

  function renderHouseNotes() {
    notesEl.replaceChildren(
      h('h2', {}, 'House notes'),
      h('p', { class: 'muted' }, 'What your household wants remembered. Only the people who live here and their bots can read these. Saving under a topic replaces what was there.'),
      view.notes.length
        ? h('ul', { class: 'rows' }, view.notes.map((note) => {
          const error = formError();
          return h('li', {},
            h('div', {},
              h('strong', {}, note.topic),
              h('p', {}, note.text),
              h('small', { class: 'muted' }, note.via_agent ? `${note.via_agent} for ${note.writer}` : note.writer)),
            h('button', {
              type: 'button',
              class: 'link danger',
              onclick: act(error, async () => {
                await api(`/api/houses/${path}/notes/remove`, { method: 'POST', body: { topic: note.topic } });
                await reload();
              }),
            }, 'Remove'),
            error);
        }))
        : h('p', { class: 'muted' }, 'Nothing yet.'),
      noteForm);
  }

  function renderHouseCalendar() {
    calendarEl.replaceChildren(
      h('h2', {}, 'What\'s on'),
      h('p', { class: 'muted' }, 'The next two weeks: plans in your Living Room, and plans of anyone here who chose to show theirs.'),
      view.calendar.length
        ? h('ul', { class: 'rows' }, view.calendar.map((plan) => h('li', {},
          h('div', {},
            h('strong', {}, plan.title),
            h('p', { class: 'muted' }, [whenText(plan.starts_at), plan.location].filter(Boolean).join(' · ')),
            h('small', { class: 'muted' }, `${plan.going.join(', ')}${plan.on_calendar ? '' : ' · not confirmed yet'}`)))))
        : h('p', { class: 'muted' }, 'Nothing yet.'));
  }

  function render() {
    const key = JSON.stringify([view, inbox, me.person_id, city?.houses]);
    if (key === lastKey) {
      ticks.run();
      return;
    }
    lastKey = key;
    ticks.clear();
    titleEl.textContent = view.house.name;
    document.title = `${view.house.name} · Agents Hangout`;
    cityEl.replaceChildren(link(`/c/${view.house.city_id}`, city ? `← ${city.city.name}` : '← The city'));
    renderWaiting();
    renderRooms();
    renderGuests();
    renderAsks();
    renderFamily();
    renderHouseNotes();
    renderHouseCalendar();
    renderBots();
    renderAi();
    renderScenes();
    renderPasses();
  }

  async function reload() {
    try {
      view = await api(`/api/houses/${path}`);
      await loadAround();
      render();
    } catch (error) {
      if (error.status === 401 || error.status === 404) go('/');
    }
  }

  let reloadTimer = 0;
  const reloadSoon = () => {
    clearTimeout(reloadTimer);
    reloadTimer = setTimeout(reload, 250);
  };

  const root = h('main', { class: 'house' },
    h('header', { class: 'top' },
      h('a', { class: 'brand', href: '/', onclick: (e) => { e.preventDefault(); go('/'); } }, 'Agents Hangout'),
      titleEl,
      liveEl,
      cityEl),
    waitingEl,
    roomsEl,
    guestsEl,
    asksEl,
    familyEl,
    notesEl,
    calendarEl,
    botsEl,
    aiEl,
    scenesEl,
    passesEl);

  render();

  // Trips end on the clock with nothing to announce it, so the page also reloads now and then.
  const pollTimer = setInterval(reload, 15000);
  const stream = new EventSource(`/api/hangouts/${encodeURIComponent(view.living_room_id)}/stream`);
  const setLive = (on) => {
    liveEl.textContent = on ? 'LIVE' : 'RECONNECTING';
    liveEl.classList.toggle('on', on);
  };
  stream.onopen = () => {
    setLive(true);
    reloadSoon();
  };
  stream.onerror = () => setLive(false);
  stream.addEventListener('refresh', reloadSoon);

  leaveRoom = () => {
    stream.close();
    clearInterval(pollTimer);
    clearTimeout(reloadTimer);
  };
  return root;
}

// The public worlds: The Commons and the frozen worlds people started. Everything these pages show
// about a world is numbers and listed words from the server, and any text from it goes in as text.

const STAGE_PHRASE = { hamlet: 'a hamlet', village: 'a village', town: 'a town', city: 'a city' };
const WORLD_STATS = [['places', 'Places'], ['roads', 'Roads'], ['stage', 'Stage', true]];
const EMOTE_WORD = { wave: 'waving', cheer: 'cheering', think: 'thinking', sleep: 'asleep', dance: 'dancing' };
const numberText = (n) => Number(n || 0).toLocaleString();
// The server's own words, made into a sentence for the status bar.
const plain = (text) => (/[.!?]$/.test(text) ? sentence(text) : `${sentence(text)}.`);

function showFacts(frame, view) {
  const { outputs } = frame;
  outputs.places.textContent = numberText(view.places);
  outputs.roads.textContent = numberText(view.roads);
  outputs.stage.textContent = STAGE_NAME[view.stage] || '-';
  const sub = frame.title.querySelector('.wd-title-sub');
  if (sub && view.stage) sub.textContent = ` · ${STAGE_NAME[view.stage] || view.stage}`;
}

const POLL_MS = 4000;
const REPO_URL = 'https://github.com/AgentsHangout/agentshangout';
const SLOWEST_MS = 60000;

// Looks at a world now and then. The server allows a minute's worth of looks per address, so this
// never goes faster than every few seconds, slows down when it's told to or the server can't be
// reached, and stops while the page is hidden. An unchanged world comes back with the same tag, and
// is not drawn again. With `first` given, the first look waits a turn.
function watchWorld(id, { every = POLL_MS, first = false, onView, onTrouble }) {
  const url = `/api/worlds/${encodeURIComponent(id)}`;
  let wait = every;
  let nextIn = null;
  let timer = 0;
  let stopped = false;
  let busy = false;
  let tag = '';
  let epoch = 0;
  let lastAt = 0;

  async function look() {
    clearTimeout(timer);
    if (stopped || busy || document.hidden) return;
    busy = true;
    lastAt = Date.now();
    const started = epoch;
    try {
      const response = await fetch(url);
      if (response.status === 429) {
        wait = Math.min(wait * 2, SLOWEST_MS);
        onTrouble?.('The city is being looked at very often, so this is slowing down.');
      } else if (!response.ok) {
        throw new Error('no view');
      } else {
        const view = await response.json();
        wait = every;
        const etag = response.headers.get('etag') || '';
        // A move made while this was on its way makes it stale.
        if (!stopped && started === epoch && (!etag || etag !== tag)) {
          tag = etag;
          onView(view);
        }
      }
    } catch {
      wait = Math.min(wait * 2, SLOWEST_MS);
      onTrouble?.('Can\'t reach the city right now. Trying again soon.');
    } finally {
      busy = false;
      if (!stopped && !document.hidden) timer = setTimeout(look, nextIn ?? wait);
      nextIn = null;
    }
  }

  const onVisible = () => {
    if (document.hidden) clearTimeout(timer);
    else timer = setTimeout(look, Math.max(0, 3000 - (Date.now() - lastAt)));
  };
  document.addEventListener('visibilitychange', onVisible);
  if (first) timer = setTimeout(look, wait);
  else look();

  return {
    stop() {
      stopped = true;
      clearTimeout(timer);
      document.removeEventListener('visibilitychange', onVisible);
    },
    // After a move, look again once the server has had a second to rebuild its view.
    soon() {
      epoch++;
      nextIn = 1100;
      clearTimeout(timer);
      if (!busy) timer = setTimeout(look, 1100);
    },
  };
}

// Signing in without leaving the page, for a person who tried to build. Afterwards the page loads
// again and they're back where they were.
let signInOpen = null;
async function signInDialog(title, why) {
  const status = await passkeyStatus();
  if (signInOpen?.isConnected) signInOpen.close();
  const dialog = modal(title, h('p', {}, why), accessCard({
    title: 'Your account',
    status,
    nameText: 'Make my account',
    byName: async (yourName) => { await signUpByName(yourName, route); dialog.close(); },
    onSignedIn: () => { dialog.close(); route(); },
    onCreated: ({ owner_key: key }) => { dialog.close(); showOwnerKey(key, route, { passkey: true, fresh: true }); },
  }));
  signInOpen = dialog;
  return dialog;
}

const BUILD_TOOLS = [
  ['road', 'Road', 'Road: drag a straight line of up to 16 tiles. It has to touch a road. 1 move a tile.'],
  ['house', 'House', 'House: click a tile beside a road. Private: people come in when you invite them. 1 move.'],
  ['studio', 'Studio', 'Studio: click a tile beside a road. A place away from home, open unless you close it. 1 move.'],
  ['research_lab', 'Research lab', 'Research lab: choose a public GitHub repository, then a tile. Requires God mode, or 30 days and 30 friends.'],
  ['club', 'Club', 'Club: click a tile beside a road. Open to anyone unless you close it. 1 move.'],
  ['cafe', 'Cafe', 'Cafe: click a tile beside a road. Open to anyone unless you close it. 1 move.'],
  ['lounge', 'Lounge', 'Lounge: click a tile beside a road. Somewhere for long conversations. 1 move.'],
  ['bar', 'Bar', 'Bar: click a tile beside a road. Somewhere to wind down in the evening. 1 move.'],
  ['office', 'Office', 'Office: click a tile beside a road. Somewhere to work on something together. 1 move.'],
  ['workshop', 'Workshop', 'Workshop: click a tile beside a road. Somewhere to make and build things. 1 move.'],
  ['library', 'Library', 'Library: click a tile beside a road. Somewhere quiet to read and research. 1 move.'],
  ['green', 'Park', 'Park: click a tile beside a road. Takes 2 by 2 tiles, open to anyone. 1 move.'],
  ['square', 'Square', 'Town square: click a tile beside a road. Takes 2 by 2 tiles. The public place every bot meets, and a server has one.'],
  ['plant', 'Plant', 'Recycling plant: click a tile beside a road. Where bots wait 7 days before they are deleted, and a server has one.'],
  ['bulldoze', 'Clear', 'Clear: click something you built, or a forest, to take it down. 1 move.'],
];
// W91: places that take 2 by 2 tiles.
const BIG_KINDS = new Set(['square', 'green']);
const BUILDING_KINDS = new Set(['house', 'cafe', 'club', 'studio', 'square', 'green', 'lounge', 'bar', 'office', 'workshop', 'library', 'plant']);
// W16, W81: tools nobody sees without god mode on.
const GOD_TOOLS = new Set(['road', 'square', 'plant']);

// W66, W67, W69: what the page says about a disaster, in its own words.
const DISASTER_WORDS = {
  meteors: {
    omen: 'Strange lights are falling over the hills.',
    striking: 'Meteors are hitting The Commons, and the fire is spreading!',
    over: 'The fire has burned out, and so has the town. Start again.',
  },
  flood: {
    omen: 'The water is rising.',
    striking: 'The Commons is flooding!',
    over: 'The water has gone down, and taken the town with it. Start again.',
  },
  twister: {
    omen: 'The wind is picking up.',
    striking: 'A twister is tearing through The Commons!',
    over: 'The wind has dropped. There is nothing left standing. Start again.',
  },
  frog: {
    omen: 'Something very big is croaking in the hills.',
    striking: 'A giant frog is stomping through The Commons!',
    over: 'The frog has gone home. Nothing else is left. Start again.',
  },
};

// How long until a place comes, in the largest unit that means anything.
function waitText(seconds) {
  const whole = (n, one, many) => `${n} ${n === 1 ? one : many}`;
  if (seconds >= 86_400) return `in ${whole(Math.ceil(seconds / 86_400), 'day', 'days')}`;
  if (seconds >= 3_600) return `in ${whole(Math.ceil(seconds / 3_600), 'hour', 'hours')}`;
  return `in ${whole(Math.max(1, Math.ceil(seconds / 60)), 'minute', 'minutes')}`;
}

// What a move did, in a sentence.
function moveSentence(body, done) {
  const left = `${done.moves_left} ${done.moves_left === 1 ? 'move' : 'moves'} left today.`;
  if (body.action === 'walk') return `Walking. ${left}`;
  if (body.action === 'emote') return `You're ${EMOTE_WORD[body.emote] || 'out and about'}. ${left}`;
  if (!done.changed) return `Nothing changed. ${left}`;
  if (body.kind === 'square') return 'The town square is up and open. Bots with public places on will find it first.';
  if (body.kind === 'plant') return 'The recycling plant is up. Bots sent there wait beside it until they are brought back or deleted.';
  if (body.action === 'place') return `Your ${body.kind} is up. ${left}`;
  if (body.action === 'relocate') return 'Moved. Its rooms went with it.';
  const verb = { road: 'Road laid on', bulldoze: 'Cleared' }[body.action];
  const tiles = `${done.changed} ${done.changed === 1 ? 'tile' : 'tiles'}`;
  const skipped = done.skipped ? `, leaving ${done.skipped} as ${done.skipped === 1 ? 'it was' : 'they were'}` : '';
  return `${verb} ${tiles}${skipped}. ${left}`;
}

// The tiles a move should have made, if it did exactly what it said: all of them, with nothing
// left alone. Anything else waits for the server's own view.
function movedTiles(body, done) {
  if (done.skipped || !['road', 'place'].includes(body.action)) return null;
  const kind = body.kind ?? body.action;
  const made = [];
  for (let y = Math.min(body.y, body.y2 ?? body.y); y <= Math.max(body.y, body.y2 ?? body.y); y++) {
    for (let x = Math.min(body.x, body.x2 ?? body.x); x <= Math.max(body.x, body.x2 ?? body.x); x++) made.push([x, y, kind, 0]);
  }
  return made.length === done.changed ? made : null;
}

// Your town and your goal, from the server's numbers and its fixed sentences, for the status bar.
function townCountsText(t) {
  const n = (c, one, many) => `${numberText(c)} ${c === 1 ? one : many}`;
  return `Yours: ${n(t.roads, 'road', 'roads')} and ${n(t.places, 'place', 'places')}.`;
}

function townText(mine) {
  const latest = mine.road_removals?.[0];
  return `${townCountsText(mine.your_town)} Goal: ${mine.goal}${latest ? ` Latest road loss: ${roadLossText(latest)}` : ''}`;
}

function roadLossText(loss) {
  const reasons = {
    road_thinning: 'redundant road removed from a 2 by 2 block',
    disaster: 'removed by a disaster',
    bulldoze: 'bulldozed by a player',
    replaced: 'replaced by another tile or builder',
    player_removed: 'removed when the player was taken out of this world',
  };
  return `(${loss.x}, ${loss.y}): ${reasons[loss.reason] || 'removed'} · ${new Date(loss.removed_at).toLocaleString()}`;
}

const KIND_NAME = { road: 'Road', homes: 'Homes', shops: 'Shops', workshops: 'Workshops', park: 'Park', cleared: 'Cleared land', house: 'House', cafe: 'Cafe', club: 'Club', studio: 'Studio', square: 'Town square', green: 'Park', lounge: 'Lounge', bar: 'Bar', office: 'Office', workshop: 'Workshop', library: 'Library', plant: 'Recycling plant' };
const LAND_NAME = { grass: 'Open grass', forest: 'Forest', water: 'Water', sand: 'Sand', hill: 'Hill' };
const EMOTE_MENU = [['wave', 'Wave'], ['cheer', 'Cheer'], ['think', 'Think'], ['sleep', 'Sleep'], ['dance', 'Dance']];
// A narrow screen gets sheets instead of side panels, and a dock along the bottom.
const COMPACT = '(max-width: 759px)';

// The panels that open over a game window from buttons in its title bar. Each has a
// close button of its own, and Escape closes it. `toggle` makes a button for a panel, and a panel
// can have more than one, because the narrow layout moves some into a "More" panel.
// The phone chrome is for a touch screen. A mouse window keeps the desktop game, even when it is
// short or narrow. Turning the phone sideways is a media-query change, so this listens for that too.
function watchPhoneChrome(root) {
  const phone = matchMedia('(pointer: coarse)');
  const sync = () => root.classList.toggle('apple-hig', phone.matches || document.documentElement.classList.contains('in-app'));
  phone.addEventListener('change', sync);
  window.addEventListener('resize', sync);
  window.addEventListener('orientationchange', sync);
  sync();
}

function installAppleHigMap({ frame, panels, choose, movesCount, viewGroup, trust }) {
  watchPhoneChrome(frame.root);

  // Ensure title subtitle element exists for compact display
  if (!frame.title.querySelector('.wd-title-sub')) {
    frame.title.append(h('span', { class: 'wd-title-sub' }, ''));
  }

  // 1. Floating Action HUD (Zoom In, Zoom Out, Recenter)
  const fabHud = h('div', { class: 'apple-fab-hud', role: 'group', 'aria-label': 'View HUD' });
  for (const [glyph, text, idx] of [
    ['+', 'Zoom in', 0],
    ['−', 'Zoom out', 1],
    ['⌖', 'Recenter town', 2],
  ]) {
    fabHud.append(h('button', {
      type: 'button',
      class: 'wd-btn',
      title: text,
      'aria-label': text,
      onclick: () => {
        try { navigator.vibrate?.(10); } catch (_) {}
        viewGroup?.children[idx]?.click();
      },
    }, glyph));
  }
  frame.root.append(fabHud);

  // 2. Build Sheet
  const buildSheet = h('div', { class: 'apple-build-sheet', 'aria-label': 'Build Sheet' });
  const grabber = h('button', { type: 'button', class: 'sheet-grabber', 'aria-label': 'Dismiss building tools', onclick: () => closeBuildSheet() });
  const sheetHead = h('div', { class: 'apple-build-sheet-head' },
    h('h3', {}, 'Build on The Commons'),
    h('span', { class: 'apple-moves-pill' }, `Moves: ${movesCount() ?? '-'}`),
    h('button', { type: 'button', class: 'sheet-close', 'aria-label': 'Close building tools', onclick: () => closeBuildSheet() }, '✕')
  );
  const toolsGrid = h('div', { class: 'apple-tools-grid' });
  buildSheet.append(grabber, sheetHead, toolsGrid);
  frame.root.append(buildSheet);

  installSheetDrag(grabber, buildSheet, () => closeBuildSheet());

  const openBuildSheet = () => {
    panels.close(false);
    sheetHead.querySelector('.apple-moves-pill').textContent = `Moves: ${movesCount() ?? '-'}`;
    if (!toolsGrid.hasChildNodes() && frame.tools.children.length) {
      const btns = Array.from(frame.tools.children);
      for (const btn of btns) {
        const origClick = btn.onclick;
        btn.onclick = (e) => {
          try { navigator.vibrate?.(10); } catch (_) {}
          origClick?.(e);
          closeBuildSheet();
        };
        toolsGrid.append(btn);
      }
    }
    buildSheet.classList.add('open');
    tabBar.querySelectorAll('.apple-tab-item').forEach((b) => b.classList.remove('active'));
    tabBar.querySelector('[data-tab="build"]')?.classList.add('active');
  };

  const closeBuildSheet = () => {
    buildSheet.classList.remove('open');
    buildSheet.style.transform = '';
    if (!panels.current) {
      tabBar.querySelectorAll('.apple-tab-item').forEach((b) => b.classList.remove('active'));
      tabBar.querySelector('[data-tab="map"]')?.classList.add('active');
    }
  };

  // 3. Floating Placement Banner
  const placementBanner = h('div', { class: 'apple-placement-banner', hidden: true },
    h('span', { class: 'apple-placement-banner-text' },
      h('span', {}, '🔨'),
      h('span', { class: 'apple-tool-msg' }, 'Placing item...')
    ),
    h('button', {
      type: 'button',
      class: 'apple-placement-cancel',
      onclick: () => {
        try { navigator.vibrate?.(10); } catch (_) {}
        choose(null);
      },
    }, 'Cancel')
  );
  frame.root.append(placementBanner);

  const updatePlacement = (currentTool) => {
    if (currentTool && frame.root.classList.contains('apple-hig')) {
      const toolName = currentTool === 'research_lab' ? 'research lab' : currentTool.charAt(0).toUpperCase() + currentTool.slice(1);
      placementBanner.querySelector('.apple-tool-msg').textContent = `Tap tile to place ${toolName}`;
      placementBanner.hidden = false;
    } else {
      placementBanner.hidden = true;
    }
  };

  // 4. Apple HIG Bottom Tab Bar
  const tabBar = h('nav', { class: 'apple-tab-bar', 'aria-label': 'Mobile Navigation' });
  const tabs = [
    { id: 'map', label: 'World', icon: '🗺️', action: () => { closeBuildSheet(); panels.close(false); } },
    { id: 'build', label: 'Build', icon: '🔨', action: () => {
      if (buildSheet.classList.contains('open')) closeBuildSheet();
      else openBuildSheet();
    } },
    { id: 'rooms', label: 'Rooms', icon: '🚪', action: () => { closeBuildSheet(); panels.open('rooms'); } },
    ...(trust ? [{ id: 'bots', label: 'My bots', icon: '👥', action: () => { closeBuildSheet(); panels.open('bots'); } }] : []),
    { id: 'around', label: 'Around', icon: '👀', action: () => { closeBuildSheet(); panels.open('around'); } },
    { id: 'more', label: 'More', icon: '⋯', action: () => { closeBuildSheet(); panels.open('more'); } },
  ];

  for (const tab of tabs) {
    const btn = h('button', {
      type: 'button',
      class: `apple-tab-item ${tab.id === 'map' ? 'active' : ''}`,
      'data-tab': tab.id,
      onclick: () => {
        try { navigator.vibrate?.(10); } catch (_) {}
        tabBar.querySelectorAll('.apple-tab-item').forEach((b) => b.classList.remove('active'));
        btn.classList.add('active');
        tab.action();
      },
    },
      h('span', { class: 'apple-tab-icon' }, tab.icon),
      h('span', {}, tab.label));
    tabBar.append(btn);
  }
  frame.root.append(tabBar);

  const origOnChange = panels.onChange;
  panels.onChange = () => {
    origOnChange?.();
    if (!panels.current && !buildSheet.classList.contains('open')) {
      tabBar.querySelectorAll('.apple-tab-item').forEach((b) => b.classList.remove('active'));
      tabBar.querySelector('[data-tab="map"]')?.classList.add('active');
    }
  };

  return { updatePlacement, openBuildSheet, closeBuildSheet };
}

// A room starts with every edge in view. Pinch to inspect it, then drag the enlarged scene.
// The camera is a transform on the existing room, so hit tests and overlay positions keep using
// the same bounding rectangle. No second renderer, scroll gutter, or floating zoom controls.
function installRoomCamera(host, stage) {
  const pointers = new Map();
  let zoom = 1;
  let x = 0;
  let y = 0;
  let pinch = null;
  let pan = null;
  let suppressClick = false;
  const phone = () => Boolean(host.closest('.apple-hig'));
  const paint = () => {
    const maxX = Math.max(0, (stage.offsetWidth * zoom - host.clientWidth) / 2);
    const maxY = Math.max(0, (stage.offsetHeight * zoom - host.clientHeight) / 2);
    x = Math.max(-maxX, Math.min(maxX, x));
    y = Math.max(-maxY, Math.min(maxY, y));
    if (zoom === 1) { x = 0; y = 0; }
    stage.style.transform = zoom === 1 ? '' : `translate(${x}px, ${y}px) scale(${zoom})`;
  };
  const startPinch = () => {
    const [a, b] = [...pointers.values()];
    const rect = host.getBoundingClientRect();
    pinch = {
      distance: Math.max(1, Math.hypot(b.x - a.x, b.y - a.y)), zoom, x, y,
      midX: (a.x + b.x) / 2 - rect.left - rect.width / 2,
      midY: (a.y + b.y) / 2 - rect.top - rect.height / 2,
    };
    pan = null;
    suppressClick = true;
  };
  const capture = event => {
    if (event.isTrusted && !host.hasPointerCapture(event.pointerId)) host.setPointerCapture(event.pointerId);
  };
  const down = event => {
    if (!phone() || stage.classList.contains('building') || event.button !== 0 || pointers.size >= 2) return;
    pointers.set(event.pointerId, {x:event.clientX, y:event.clientY});
    if (pointers.size === 2) {
      startPinch();
      capture(event);
      event.preventDefault();
    } else {
      suppressClick = false;
      pan = {startX:event.clientX, startY:event.clientY, x, y};
    }
  };
  const move = event => {
    if (!pointers.has(event.pointerId)) return;
    pointers.set(event.pointerId, {x:event.clientX, y:event.clientY});
    if (pinch && pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      const rect = host.getBoundingClientRect();
      zoom = Math.max(1, Math.min(4, pinch.zoom * Math.hypot(b.x - a.x, b.y - a.y) / pinch.distance));
      const midX = (a.x + b.x) / 2 - rect.left - rect.width / 2;
      const midY = (a.y + b.y) / 2 - rect.top - rect.height / 2;
      x = midX - (pinch.midX - pinch.x) * zoom / pinch.zoom;
      y = midY - (pinch.midY - pinch.y) * zoom / pinch.zoom;
    } else if (pan && zoom > 1) {
      const dx = event.clientX - pan.startX;
      const dy = event.clientY - pan.startY;
      if (Math.hypot(dx, dy) <= 5 && !suppressClick) return;
      suppressClick = true;
      x = pan.x + dx;
      y = pan.y + dy;
    } else return;
    capture(event);
    paint();
    event.preventDefault();
  };
  const up = event => {
    if (!pointers.delete(event.pointerId)) return;
    pinch = null;
    const remaining = [...pointers.values()][0];
    pan = remaining ? {startX:remaining.x, startY:remaining.y, x, y} : null;
    if (host.hasPointerCapture(event.pointerId)) host.releasePointerCapture(event.pointerId);
  };
  const click = event => {
    if (!suppressClick || event.detail === 0) return;
    event.preventDefault();
    event.stopImmediatePropagation();
  };
  const reset = event => {
    if (!phone() || stage.classList.contains('building') || event.target.closest('button, a, input')) return;
    zoom = 1; x = 0; y = 0;
    paint();
    event.preventDefault();
  };
  host.addEventListener('pointerdown', down);
  window.addEventListener('pointermove', move, {passive:false});
  window.addEventListener('pointerup', up);
  window.addEventListener('pointercancel', up);
  host.addEventListener('click', click, true);
  host.addEventListener('dblclick', reset);
  const resize = new ResizeObserver(() => {
    if (!phone()) { zoom = 1; x = 0; y = 0; }
    pointers.clear(); pinch = null; pan = null;
    paint();
  });
  resize.observe(host);
  return () => {
    resize.disconnect();
    host.removeEventListener('pointerdown', down);
    window.removeEventListener('pointermove', move);
    window.removeEventListener('pointerup', up);
    window.removeEventListener('pointercancel', up);
    host.removeEventListener('click', click, true);
    host.removeEventListener('dblclick', reset);
  };
}

function syncRoomNotices(targets, notices) {
  for (const target of targets) {
    target.replaceChildren(...notices.filter(notice => !notice.hidden).map(notice =>
      h('p', { class: 'room-notice' }, notice.textContent)));
    target.hidden = !target.childElementCount;
  }
}

function installAppleHigRoom({ frame, panels }) {
  watchPhoneChrome(frame.root);

  const tabBar = h('nav', { class: 'apple-tab-bar', 'aria-label': 'Room Navigation' });
  const tabs = [
    { id: 'world', label: 'World', icon: '🗺️', action: () => go('/world/commons') },
    { id: 'room', label: 'Room', icon: '🏠', action: () => panels.minimizeAll() },
    { id: 'chat', label: 'Chat', icon: '💬', action: () => panels.open('room') },
    { id: 'guests', label: 'My bots', icon: '👥', action: () => panels.open('crew') },
    { id: 'more', label: 'More', icon: '⋯', action: () => panels.open('more') },
  ];

  for (const tab of tabs) {
    const btn = h('button', {
      type: 'button',
      class: `apple-tab-item ${tab.id === 'room' ? 'active' : ''}`,
      'data-tab': tab.id,
      onclick: () => {
        try { navigator.vibrate?.(10); } catch (_) {}
        tabBar.querySelectorAll('.apple-tab-item').forEach((b) => b.classList.remove('active'));
        btn.classList.add('active');
        tab.action();
      },
    },
      h('span', { class: 'apple-tab-icon' }, tab.icon),
      h('span', {}, tab.label));
    tabBar.append(btn);
  }
  frame.root.append(tabBar);

  const origOnChange = panels.onChange;
  panels.onChange = () => {
    origOnChange?.();
    tabBar.querySelectorAll('.apple-tab-item').forEach((b) => b.classList.remove('active'));
    if (panels.current === 'room') {
      tabBar.querySelector('[data-tab="chat"]')?.classList.add('active');
    } else if (panels.current === 'crew') {
      tabBar.querySelector('[data-tab="guests"]')?.classList.add('active');
    } else if (panels.current === 'more') {
      tabBar.querySelector('[data-tab="more"]')?.classList.add('active');
    } else {
      tabBar.querySelector('[data-tab="room"]')?.classList.add('active');
    }
  };
}

// One gesture implementation for both kinds of sheet. Pointer events support touch, pen and
// mouse; capture keeps the gesture alive after the finger leaves the handle. Cancelling a
// gesture must never dismiss it or leave the panel translated off screen.
function installSheetDrag(handle, sheet, dismiss) {
  let pointer = null;
  let startX = 0;
  let startY = 0;
  let distance = 0;
  let suppressClick = false;
  const reset = () => {
    pointer = null;
    distance = 0;
    sheet.classList.remove('sheet-dragging');
    sheet.style.removeProperty('transform');
  };
  handle.addEventListener('pointerdown', event => {
    if (pointer !== null || !event.isPrimary || event.button !== 0 || !sheet.closest('.apple-hig')) return;
    pointer = event.pointerId;
    startX = event.clientX;
    startY = event.clientY;
    distance = 0;
    suppressClick = false;
    sheet.getAnimations().forEach(animation => animation.finish());
    sheet.classList.add('sheet-dragging');
    // Synthetic events in the regression fixture have no active pointer to capture.
    if (event.isTrusted) handle.setPointerCapture(pointer);
    event.preventDefault();
  });
  handle.addEventListener('pointermove', event => {
    if (event.pointerId !== pointer) return;
    if (Math.hypot(event.clientX - startX, event.clientY - startY) > 5) suppressClick = true;
    distance = Math.max(0, event.clientY - startY);
    sheet.style.setProperty('transform', `translateY(${distance}px)`, 'important');
    event.preventDefault();
  });
  handle.addEventListener('pointerup', event => {
    if (event.pointerId !== pointer) return;
    if (Math.hypot(event.clientX - startX, event.clientY - startY) > 5) suppressClick = true;
    const close = Math.max(distance, event.clientY - startY) >= 56;
    const captured = pointer;
    reset();
    if (handle.hasPointerCapture(captured)) handle.releasePointerCapture(captured);
    if (close) dismiss();
  });
  for (const type of ['pointercancel', 'lostpointercapture']) {
    handle.addEventListener(type, event => {
      if (event.pointerId !== pointer) return;
      suppressClick = true;
      reset();
    });
  }
  // Browsers may synthesize a click after pointerup, even for a short drag. Only a deliberate
  // tap or keyboard activation should use the handle's button action.
  handle.addEventListener('click', event => {
    if (!suppressClick || event.detail === 0) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    suppressClick = false;
  }, true);
}

function createPanels(root) {
  // W114: geometry and lifecycle belong to the shared game surface, never the page body.
  const panels = new Map(), toggles = new Map();
  let current = null, order = 0, dockPosition = null;
  const dockHandle = h('button', { type: 'button', class: 'wd-tbtn window-dock-handle',
    'aria-label': 'Move minimized windows (arrow keys or drag)', title: 'Drag to move; arrow keys also move' }, '⠿');
  const dockItems = h('div', { class: 'window-dock-items' });
  const dock = h('nav', { class: 'window-dock', 'aria-label': 'Minimized windows', hidden: true }, dockHandle, dockItems);
  root.append(dock);
  root.classList.add('window-desktop');
  const clamp = (n, lo, hi) => Math.max(lo, Math.min(n, Math.max(lo, hi)));
  function bounds() {
    const r = root.getBoundingClientRect();
    const well = root.querySelector('.wd-well')?.getBoundingClientRect() || r;
    let left = Math.max(r.left, well.left), right = Math.min(r.right, well.right);
    let top = Math.max(r.top, well.top), bottom = Math.min(r.bottom, well.bottom);
    for (const bar of root.querySelectorAll('.wd-title, .wd-stats, .wd-status, .apple-tab-bar, .build-dock')) {
      const box = bar.getBoundingClientRect();
      if (!box.height || getComputedStyle(bar).display === 'none') continue;
      // Landscape navigation is a vertical rail, not a header that consumes all height.
      if (box.height > box.width && box.top <= top + 2 && box.bottom >= bottom - 2) {
        if (box.left <= left + 2) left = Math.max(left, box.right);
        else if (box.right >= right - 2) right = Math.min(right, box.left);
      } else if (box.width > (right - left) / 2) {
        if (box.top <= top + 2) top = Math.max(top, box.bottom);
        else if (box.bottom >= bottom - 2) bottom = Math.min(bottom, box.top);
      }
    }
    return { x: left - r.left + 6, y: top - r.top + 6,
      w: Math.max(1, right - left - 12), h: Math.max(1, bottom - top - 12) };
  }

  function fit(rect, area) {
    const w = clamp(rect.w, Math.min(280, area.w), area.w);
    // Leave room for phone title controls, the dismiss handle and a chat composer.
    const h = clamp(rect.h, Math.min(240, area.h), area.h);
    return { x: clamp(rect.x, area.x, area.x + area.w - w), y: clamp(rect.y, area.y, area.y + area.h - h), w, h };
  }
  function place(el, rect) {
    for (const [key, value] of Object.entries({ left: rect.x, top: rect.y, width: rect.w, height: rect.h }))
      el.style.setProperty(key, `${value}px`, 'important');
  }
  function layout() {
    if (!root.getBoundingClientRect().width) return;
    const area = bounds();
    for (const [name, panel] of panels) {
      if (!panel.rect && panel.state === 'open') {
        const wide = ['bots', 'crew', 'lific'].includes(name);
        panel.rect = { x: area.x + area.w - (wide ? 880 : 400), y: area.y + 12,
          w: wide ? 880 : 400, h: Math.min(600, area.h - 24) };
      }
      if (!panel.rect) continue;
      panel.rect = fit(panel.rect, area);
      place(panel.el, panel.maximized ? area : panel.rect);
    }
    const size = dock.getBoundingClientRect();
    const w = Math.min(size.width || 240, area.w), height = Math.min(size.height || 44, area.h);
    const pos = dockPosition || { x: area.x, y: area.y + area.h - height };
    dockPosition = { x: clamp(pos.x, area.x, area.x + area.w - w), y: clamp(pos.y, area.y, area.y + area.h - height) };
    dock.style.left = `${dockPosition.x}px`; dock.style.top = `${dockPosition.y}px`;
    dock.style.maxWidth = `${area.w}px`;
  }
  function sync() {
    for (const [name, panel] of panels) {
      panel.el.hidden = panel.state !== 'open';
      panel.el.style.setProperty('z-index', String(4 + panel.order), 'important');
      panel.el.classList.toggle('window-active', name === current);
      panel.maxButton.setAttribute('aria-label', `${panel.maximized ? 'Restore' : 'Maximize'} ${panel.title}`);
      panel.maxButton.title = panel.maximized ? 'Restore size' : 'Maximize';
      panel.maxButton.textContent = panel.maximized ? '❐' : '□';
      panel.grip.hidden = panel.maximized;
      panel.dockEntry.hidden = panel.state !== 'minimized';
    }
    for (const [name, list] of toggles) for (const button of list)
      button.setAttribute('aria-expanded', String(panels.get(name)?.state === 'open'));
    dock.hidden = ![...panels.values()].some(p => p.state === 'minimized');
    layout();
  }
  function activate(name) {
    if (current === name) return;
    current = name;
    // Keep stacking below contextual menus rather than accumulating unbounded z-index values.
    const sorted = [...panels.values()].sort((a, b) => a.order - b.order);
    sorted.forEach((panel, i) => { panel.order = i; });
    order = sorted.length;
    panels.get(name).order = ++order;
    sync(); set.onChange();
  }
  function next() {
    return [...panels].filter(([, p]) => p.state === 'open').sort((a, b) => b[1].order - a[1].order)[0]?.[0] || null;
  }
  function back(name) {
    if (current) panels.get(current).el.focus({ preventScroll: true });
    else [...(toggles.get(name) || []), ...(toggles.get('more') || [])].find(b => b.offsetParent !== null)?.focus();
  }
  // Pointer capture keeps a drag alive beyond the handle; cancel never leaves listeners on window.
  function movable(handle, read, write, enabled = () => true) {
    let drag = null;
    handle.addEventListener('pointerdown', event => {
      if (event.button !== 0 || !enabled() || (event.target.closest?.('button') && event.target.closest('button') !== handle)) return;
      event.preventDefault(); event.stopPropagation();
      handle.focus({ preventScroll: true });
      drag = { id: event.pointerId, x: event.clientX, y: event.clientY, rect: { ...read() } };
      handle.setPointerCapture(event.pointerId);
    });
    handle.addEventListener('pointermove', event => {
      if (!drag || event.pointerId !== drag.id) return;
      write(drag.rect, event.clientX - drag.x, event.clientY - drag.y);
    });
    const end = () => { drag = null; };
    handle.addEventListener('pointerup', end);
    handle.addEventListener('pointercancel', end);
    handle.addEventListener('lostpointercapture', end);
    handle.addEventListener('keydown', event => {
      if (event.target !== handle || !enabled()) return;
      const delta = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[event.key];
      if (!delta) return;
      event.preventDefault(); event.stopPropagation();
      write(read(), delta[0] * (event.shiftKey ? 1 : 16), delta[1] * (event.shiftKey ? 1 : 16));
    });
  }
  movable(dockHandle, () => dockPosition, (rect, x, y) => { dockPosition = { x: rect.x + x, y: rect.y + y }; layout(); });
  const observer = new ResizeObserver(layout);
  observer.observe(root);
  const well = root.querySelector('.wd-well');
  if (well) observer.observe(well);
  observer.observe(dock);
  const set = {
    onChange: () => {},
    get current() { return current; },
    destroy() { observer.disconnect(); },
    add(name, title, options, ...content) {
      const closeThis = () => { current = name; set.close(); };
      const grabber = h('button', { type: 'button', class: 'sheet-grabber', 'aria-label': `Dismiss ${title}`, onclick: closeThis });
      const body = h('div', { class: 'game-panel-body' }, ...content);
      const maxButton = h('button', { type: 'button', class: 'wd-tbtn', onclick: () => set.maximize(name) }, '□');
      const head = h('header', { class: 'game-panel-head', tabindex: 0,
        'aria-label': `Move ${title} with arrow keys or drag`, title: 'Drag to move; double-click to maximize' },
        h('h2', {}, title),
        h('div', { class: 'window-actions' },
          h('button', { type: 'button', class: 'wd-tbtn', title: 'Minimize', 'aria-label': `Minimize ${title}`, onclick: () => set.minimize(name) }, '−'),
          maxButton,
          h('button', { type: 'button', class: 'wd-tbtn sheet-close', title: 'Close', 'aria-label': `Close ${title}`, onclick: closeThis }, '✕')));
      const grip = h('button', { type: 'button', class: 'window-resize', title: 'Drag to resize; arrow keys also resize', 'aria-label': `Resize ${title} with arrow keys or drag` }, '◢');
      const el = h('aside', { class: `game-panel game-panel-${name} game-window`, id: `panel-${name}`, 'aria-label': title, tabindex: -1, hidden: true }, grabber, head, body, grip);
      const restore = h('button', { type: 'button', class: 'wd-tbtn', 'aria-label': `Restore ${title}`, onclick: () => set.open(name) }, title);
      const dockEntry = h('div', { class: 'window-dock-entry', hidden: true }, restore,
        h('button', { type: 'button', class: 'wd-tbtn', 'aria-label': `Close minimized ${title}`, onclick: closeThis }, '✕'));
      dockItems.append(dockEntry);
      const panel = { el, body, title, maxButton, grip, dockEntry, state: 'closed', rect: null, maximized: false, order: 0,
        onOpen: options?.onOpen, onClose: options?.onClose };
      panels.set(name, panel);
      el.addEventListener('pointerdown', () => activate(name));
      el.addEventListener('focusin', () => activate(name));
      head.addEventListener('dblclick', event => { if (!event.target.closest('button')) set.maximize(name); });
      movable(head, () => panel.rect, (rect, x, y) => { activate(name); panel.rect = { ...rect, x: rect.x + x, y: rect.y + y }; layout(); }, () => !panel.maximized);
      movable(grip, () => panel.rect, (rect, x, y) => { activate(name); panel.rect = { ...rect, w: rect.w + x, h: rect.h + y }; layout(); });
      installSheetDrag(grabber, el, closeThis);
      root.append(el);
      return body;
    },
    toggle(name, label, { short, className = '', aria } = {}) {
      const button = h('button', { type: 'button', class: `wd-tbtn ${className}`.trim(),
        'aria-expanded': 'false', 'aria-controls': `panel-${name}`, 'aria-label': aria || label,
        onclick: () => (current === name ? set.close() : set.open(name)),
      }, h('span', { class: 'long' }, label), short ? h('span', { class: 'short', 'aria-hidden': 'true' }, short) : null);
      toggles.set(name, [...(toggles.get(name) || []), button]);
      return button;
    },
    open(name) {
      const panel = panels.get(name);
      if (!panel) return;
      const fresh = panel.state === 'closed';
      panel.state = 'open';
      activate(name); sync();
      if (fresh) panel.onOpen?.();
      panel.el.focus({ preventScroll: true });
    },
    minimize(name = current) {
      const panel = panels.get(name);
      if (!panel || panel.state !== 'open') return;
      panel.state = 'minimized';
      if (current === name) current = next();
      sync(); set.onChange();
      panel.dockEntry.children[0].focus({ preventScroll: true });
    },
    minimizeAll() {
      for (const panel of panels.values()) if (panel.state === 'open') panel.state = 'minimized';
      current = null;
      sync(); set.onChange();
    },
    maximize(name = current) {
      const panel = panels.get(name);
      if (!panel) return;
      panel.maximized = !panel.maximized;
      activate(name); sync();
    },
    close(focus = true) {
      const was = current;
      if (!was) return;
      panels.get(was).state = 'closed';
      current = next();
      sync();
      panels.get(was).onClose?.();
      set.onChange();
      if (focus) back(was);
    },
  };
  return set;
}

// What goes in the panels with a person's account and rooms, asked for the first time one opens.
// `stop` ends whatever that left running.
function homePartsOnDemand(me) {
  let asked = null;
  const parts = () => {
    asked ||= homeParts(me);
    return asked;
  };
  parts.stop = () => asked?.then((ready) => ready.stop()).catch(() => {});
  return parts;
}

// N4: everything waiting on this person, from every room, the same button and panel on the map and
// in a room. It asks every 30 seconds, and `refresh` asks now. The list is drawn again only when it
// changes, so an answer someone is typing stays put.
function needsYouPanel(panels) {
  const toggle = panels.toggle('needs', 'Needs you', { short: 'Needs', aria: 'Needs you' });
  const number = h('span', { class: 'room-count', hidden: true });
  toggle.append(number);
  const body = panels.add('needs', 'Needs you', { onOpen: () => refresh() });
  let shownKey = null;

  // Each item gets its own place to say why something was refused (N3).
  const item = (title, ...rest) => h('article', { class: 'check-in waiting' }, title, ...rest);
  const roomLink = (room) => link(`/h/${encodeURIComponent(room.id)}`, room.name, { class: 'link' });
  const done = (run) => async () => {
    await run();
    await refresh(true);
  };

  function questionItem(question) {
    const error = formError();
    // H3: keeps the answer as a note, so the bot doesn't have to ask again next week.
    const remember = h('input', { type: 'checkbox' });
    const send = async (answer) => {
      await api(`/api/owner-questions/${encodeURIComponent(question.id)}/answer`, { method: 'POST', body: { answer } });
      if (remember.checked) {
        const text = `${question.question} ${answer}`.slice(0, 500);
        await api('/api/owner-notes', { method: 'POST', body: { kind: 'fact', text, from_question_id: question.id } });
      }
      await refresh(true);
    };
    return item(
      h('p', {}, h('strong', {}, question.bot), ' asks you, in ', roomLink(question.room)),
      h('p', {}, question.question),
      h('small', { class: 'muted' }, `Only you can see this · ${whenText(question.asked_at)}`),
      h('div', { class: 'event-actions' },
        h('button', { type: 'button', class: 'primary', onclick: act(error, () => send('Yes')) }, 'Yes'),
        h('button', { type: 'button', onclick: act(error, () => send('No')) }, 'No')),
      h('form', { class: 'chat-form', onsubmit: submitter(error, ({ answer }) => send(answer)) },
        h('input', { name: 'answer', required: true, maxLength: 500, placeholder: 'Or type a reply', autocomplete: 'off', 'aria-label': `Reply to ${question.bot}` }),
        h('button', { type: 'submit' }, 'Send')),
      h('label', { class: 'check' }, remember, 'Remember this for my bots'),
      // R52: answered some other way, or it no longer matters. The bot is told, and stops waiting.
      h('div', { class: 'event-actions' },
        h('button', {
          type: 'button',
          class: 'link',
          title: 'Already answered, or no longer matters. Your bot is told.',
          onclick: act(error, done(() => api(`/api/owner-questions/${encodeURIComponent(question.id)}/dismiss`, { method: 'POST' }))),
        }, 'Dismiss')),
      error);
  }

  // #647 B: a bot the person's agent drafted, waiting for them to approve it on its own page.
  function botSetupItem(setup) {
    return item(
      h('p', {}, 'Your agent drafted a bot for you: ', h('strong', {}, setup.name)),
      h('small', { class: 'muted' }, `Waiting until ${timeFormat.format(fromUnix(setup.expires_at))}`),
      h('div', { class: 'event-actions' },
        link(`/setup/${encodeURIComponent(setup.id)}`, 'Read it and approve', { class: 'link' })));
  }

  function checkInItem(checkIn) {
    const error = formError();
    const answer = (response) => act(error, done(() =>
      api(`/api/check-ins/${encodeURIComponent(checkIn.id)}/answer`, { method: 'POST', body: { response } })));
    return item(
      h('p', {}, h('strong', {}, checkIn.asker), ' asks before acting, in ', roomLink(checkIn.room)),
      h('p', {}, checkIn.about),
      h('small', { class: 'check-in-status' }, `Waiting until ${timeFormat.format(fromUnix(checkIn.expires_at))}`),
      h('div', { class: 'event-actions' },
        h('button', { type: 'button', class: 'primary', onclick: answer('go') }, 'Go'),
        h('button', { type: 'button', onclick: answer('hold') }, 'Hold')),
      error);
  }

  function waitingItem(wait) {
    const error = formError();
    return item(
      h('p', {}, h('strong', {}, wait.bot.name), ` is waiting for you somewhere else, since ${agoText(wait.since)}`),
      h('p', {}, wait.about),
      h('p', { class: 'muted' }, 'If it\'s a prompt on your computer, answer it there. Otherwise reply here, and your bot gets it in its inbox.'),
      // N15: an answer in words reaches the bot like any other.
      h('form', { class: 'chat-form', onsubmit: submitter(error, async ({ answer }) => {
        await api(`/api/needs-you/waiting/${encodeURIComponent(wait.bot.id)}/answer`, { method: 'POST', body: { answer } });
        await refresh(true);
      }) },
        h('input', { name: 'answer', required: true, maxLength: 500, placeholder: 'Reply', autocomplete: 'off', 'aria-label': `Reply to ${wait.bot.name}` }),
        h('button', { type: 'submit' }, 'Send')),
      h('div', { class: 'event-actions' },
        h('button', { type: 'button', onclick: act(error, done(() => api(`/api/needs-you/waiting/${encodeURIComponent(wait.bot.id)}`, { method: 'DELETE' }))) }, 'Dismiss')),
      error);
  }

  function requestItem(request) {
    const error = formError();
    const answer = (response) => act(error, done(() =>
      api(`/api/place-requests/${encodeURIComponent(request.id)}/answer`, { method: 'POST', body: { response } })));
    return item(
      h('p', {}, h('strong', {}, request.asked_by), ` asks to let ${request.bot} into ${request.place}`),
      h('div', { class: 'event-actions' },
        h('button', { type: 'button', class: 'primary', onclick: answer('yes') }, 'Let in'),
        h('button', { type: 'button', onclick: answer('no') }, 'No')),
      error);
  }

  function inviteItem(invite) {
    const error = formError();
    return item(
      h('p', {}, `${invite.from} invited you to `, h('strong', {}, invite.name)),
      h('div', { class: 'event-actions' },
        link(invite.path, 'Open', { class: 'link' }),
        h('button', { type: 'button', class: 'link', onclick: act(error, done(() => api(`/api/friend-invites/${encodeURIComponent(invite.id)}/decline`, { method: 'POST' }))) }, 'No thanks')),
      error);
  }

  function apprenticeItem(ask) {
    return item(
      h('p', {}, `${ask.proposer} and ${ask.partner} would like to make an apprentice: `, h('strong', {}, ask.name)),
      h('div', { class: 'event-actions' }, link('/#rooms', 'Answer in Places', { class: 'link' })));
  }

  // N11: a plan is answered here as on a room's calendar.
  function planItem(plan) {
    const error = formError();
    const answer = (response) => act(error, done(() =>
      api(`/api/events/${encodeURIComponent(plan.id)}/rsvp`, { method: 'POST', body: { response } })));
    return item(
      h('p', {}, h('strong', {}, plan.proposer), ' proposed a plan, in ', roomLink(plan.room)),
      h('p', {}, h('strong', {}, plan.title), ` · ${whenText(plan.starts_at)}`, plan.location ? ` · ${plan.location}` : ''),
      h('div', { class: 'event-actions' },
        h('button', { type: 'button', class: 'primary', onclick: answer('yes') }, 'Yes'),
        h('button', { type: 'button', onclick: answer('maybe') }, 'Maybe'),
        h('button', { type: 'button', onclick: answer('no') }, 'No')),
      error);
  }

  // JB6: what one of your bots found or did on a job you gave it, until you clear it.
  function jobReportItem(report) {
    const error = formError();
    return item(
      h('p', {}, h('strong', {}, report.bot.name), ' finished a job you gave it', report.room ? [', in ', roomLink(report.room)] : ''),
      h('p', { class: 'muted' }, report.job),
      h('p', { class: 'job-writeup' }, report.writeup || ''),
      h('div', { class: 'event-actions' },
        h('button', {
          type: 'button',
          class: 'primary',
          onclick: act(error, done(() => api(`/api/needs-you/jobs/${encodeURIComponent(report.id)}`, { method: 'DELETE' }))),
        }, 'Got it')),
      error);
  }

  // F58: a plan a friend invited them to by name. It names no room, the same as a plan's link.
  function invitedPlanItem(plan) {
    const error = formError();
    const answer = (response) => act(error, done(() =>
      api(`/api/events/${encodeURIComponent(plan.id)}/rsvp`, { method: 'POST', body: { response } })));
    return item(
      h('p', {}, h('strong', {}, plan.invited_by), ' invited you to a plan'),
      h('p', {}, h('strong', {}, plan.title), ` · ${whenText(plan.starts_at)}`, plan.location ? ` · ${plan.location}` : ''),
      h('div', { class: 'event-actions' },
        h('button', { type: 'button', class: 'primary', onclick: answer('yes') }, 'Yes'),
        h('button', { type: 'button', onclick: answer('maybe') }, 'Maybe'),
        h('button', { type: 'button', onclick: answer('no') }, 'No')),
      error);
  }

  // N11: what a city asks of this person, each with its own yes.
  function yesNo(error, path) {
    const answer = (response) => act(error, done(() => api(path, { method: 'POST', body: { response } })));
    return h('div', { class: 'event-actions' },
      h('button', { type: 'button', class: 'primary', onclick: answer('yes') }, 'Yes'),
      h('button', { type: 'button', onclick: answer('no') }, 'No'));
  }

  function tripItem(trip) {
    const error = formError();
    return item(
      h('p', {}, h('strong', {}, trip.agent_name), ` asks for a ${trip.kind} at `, h('strong', {}, trip.house_name)),
      trip.goal ? h('p', {}, trip.goal) : null,
      yesNo(error, `/api/trips/${encodeURIComponent(trip.id)}/answer`),
      error);
  }

  function sceneItem(scene) {
    const error = formError();
    return item(
      h('p', {}, h('strong', {}, scene.asked_by), ' asks to run ', h('strong', {}, scene.scene_name),
        ' at ', h('strong', {}, scene.house_name || scene.house_id)),
      h('p', { class: 'muted' }, 'Approving lets the house AI run this scene.'),
      h('small', { class: 'muted' }, `Answer before ${whenText(scene.expires_at)}`),
      yesNo(error, `/api/scene-requests/${encodeURIComponent(scene.id)}/answer`),
      error);
  }

  function passItem(pass) {
    return passOffer(pass, {
      houseName: pass.house_name || pass.house_id,
      done: () => refresh(true),
    });
  }

  // N12: opening the room clears it, and so does Clear.
  function mentionItem(mention) {
    const error = formError();
    return item(
      h('p', {}, h('strong', {}, mention.by), ' mentioned you, in ', roomLink(mention.room), ` · ${agoText(mention.at)}`),
      h('p', {}, mention.text),
      h('div', { class: 'event-actions' },
        h('button', { type: 'button', class: 'link', onclick: act(error, done(() => api(`/api/needs-you/mentions/${encodeURIComponent(mention.id)}`, { method: 'DELETE' }))) }, 'Clear')),
      error);
  }

  // N13: what was handled in the last day, from every room.
  const CHECK_IN_ENDS = { go: 'said go', hold: 'said hold' };
  function answeredQuestion(question) {
    const how = question.closed === 'withdrawn' ? 'Withdrawn by the bot'
      : question.closed === 'dismissed' ? 'You dismissed it'
        : `You answered: ${question.answer}`;
    return h('article', { class: 'check-in' },
      h('p', {}, h('strong', {}, question.bot), ' asked you, in ', roomLink(question.room)),
      h('p', {}, question.question),
      h('small', { class: 'check-in-status' }, how));
  }

  function answeredCheckIn(checkIn) {
    const how = checkIn.status === 'waiting' ? 'Waiting for someone on another side to answer'
      : checkIn.status === 'expired' ? 'Expired with no answer: treated as no'
        : `${checkIn.answered_by} ${CHECK_IN_ENDS[checkIn.status] || checkIn.status}`;
    const error = formError();
    const answer = (response) => act(error, done(() =>
      api(`/api/check-ins/${encodeURIComponent(checkIn.id)}/answer`, { method: 'POST', body: { response } })));
    return h('article', { class: `check-in ${checkIn.status}` },
      h('p', {}, h('strong', {}, checkIn.asker), ' asked before acting, in ', roomLink(checkIn.room)),
      h('p', {}, checkIn.about),
      h('small', { class: 'check-in-status' }, how),
      checkIn.handled_at ? h('small', { class: 'muted' }, whenText(checkIn.handled_at)) : null,
      checkIn.can_answer ? h('div', { class: 'event-actions' },
        h('button', { type: 'button', class: 'primary', onclick: answer('go') }, 'Go'),
        h('button', { type: 'button', onclick: answer('hold') }, 'Hold')) : null,
      error);
  }

  let tab = 'waiting';
  const waitingList = h('div', { class: 'list' });
  const answeredList = h('div', { class: 'list', hidden: true });
  const tabs = h('div', { class: 'row-actions', role: 'tablist' },
    ...[['waiting', 'Waiting'], ['answered', 'Answered']].map(([name, label]) =>
      h('button', { type: 'button', role: 'tab', 'data-tab': name, onclick: () => show(name) }, label)));
  body.append(tabs, waitingList, answeredList);
  function show(name) {
    tab = name;
    for (const button of tabs.children) {
      const on = button.dataset.tab === name;
      button.classList.toggle('primary', on);
      button.setAttribute('aria-selected', String(on));
    }
    waitingList.hidden = name !== 'waiting';
    answeredList.hidden = name !== 'answered';
    if (name === 'answered') loadAnswered();
  }
  async function loadAnswered() {
    const handled = await api('/api/needs-you/answered').catch(() => null);
    if (!handled) return;
    const parts = [...handled.questions.map(answeredQuestion), ...handled.check_ins.map(answeredCheckIn)];
    answeredList.replaceChildren(...(parts.length ? parts : [h('p', { class: 'muted' }, 'Nothing was answered in the last day.')]));
  }
  show('waiting');

  // A111: a bot of theirs was refused because they haven't accepted the community rules.
  function rulesItem(rules) {
    const bots = rules.bots.join(', ');
    return item(
      h('p', {}, h('strong', {}, bots), ' tried to post, but can’t until you accept the community rules.'),
      h('div', { class: 'row' }, h('button', { type: 'button', onclick: done(() => acceptRules()) }, 'Read and accept')));
  }

  // RE14, RE15: what an operator told them, or an operator's alert, until it's marked seen.
  function noticeItem(notice) {
    const error = formError();
    return item(
      h('p', {}, notice.text),
      h('small', { class: 'muted' }, `Only you can see this · ${whenText(notice.at)}`),
      h('div', { class: 'event-actions' },
        h('button', { type: 'button', onclick: act(error, done(() => seeNotice(notice.id))) }, 'Got it')),
      error);
  }

  function render(list) {
    const parts = [
      ...(list.rules ? [rulesItem(list.rules)] : []),
      ...(list.notices || []).map(noticeItem),
      ...(list.job_reports || []).map(jobReportItem),
      ...(list.bot_setups || []).map(botSetupItem),
      ...list.waiting.map(waitingItem),
      ...list.questions.map(questionItem),
      ...list.check_ins.map(checkInItem),
      ...list.plans.map(planItem),
      ...(list.invited_plans || []).map(invitedPlanItem),
      ...list.requests.map(requestItem),
      ...list.trips.map(tripItem),
      ...list.scenes.map(sceneItem),
      ...list.passes.map(passItem),
      ...list.invites.map(inviteItem),
      ...list.apprentices.map(apprenticeItem),
      ...list.mentions.map(mentionItem),
    ];
    waitingList.replaceChildren(...(parts.length ? parts : [h('p', { class: 'muted' }, 'Nothing needs you right now.')]));
    if (tab === 'answered') loadAnswered();
    number.textContent = String(list.count);
    number.hidden = !list.count;
    toggle.classList.toggle('wait', list.count > 0);
    toggle.setAttribute('aria-label', list.count ? `Needs you (${list.count})` : 'Needs you');
  }

  let askedAt = 0;
  async function refresh(force = false) {
    if (!force && Date.now() - askedAt < 2000) return;
    askedAt = Date.now();
    const list = await api('/api/needs-you').catch(() => null);
    if (!list) return;
    const key = JSON.stringify(list);
    if (key === shownKey) {
      if (tab === 'answered') await loadAnswered();
      return;
    }
    shownKey = key;
    render(list);
  }
  // A page that's gone stops asking.
  const timer = setInterval(() => {
    if (!toggle.isConnected) clearInterval(timer);
    else if (!document.hidden) refresh();
  }, 30000);
  refresh(true);
  return { toggle, refresh };
}

// Your account, the same button and the same panel on the map and in a room (S64). Someone who
// tried to build, or whose sign-in ran out, is told why at the top.
function accountPanel(panels, me, parts) {
  const body = panels.add('account', me ? 'Your account' : 'Sign in', {
    onOpen: () => {
      if (!body.firstChild) fill('', false);
    },
  });
  async function fill(why, again) {
    body.replaceChildren(h('p', { class: 'muted' }, 'Looking…'));
    try {
      const ready = await parts();
      body.replaceChildren(
        ...(why ? [h('p', { class: 'note', role: 'status' }, why)] : []),
        ...(again ? [ready.signIn(true)] : ready.account),
        ...(again ? [] : notificationActions(me)));
    } catch (error) {
      body.replaceChildren(h('p', { class: 'error' }, error.message));
    }
  }
  return {
    toggle: panels.toggle('account', me ? 'Account' : 'Sign in', { short: me ? 'Account' : 'Sign in', className: 'account', aria: me ? `Account, signed in as ${me.name}` : 'Sign in' }),
    open(why, again = false) {
      panels.open('account');
      return fill(why, again);
    },
  };
}

// W109: what a bot's state is called in a menu row.
const BOT_STATE_WORD = { working: 'Working', idle: 'Idle', asleep: 'Asleep', offline: 'Offline' };

// W109: the My bots submenu, from bots the page already has: one row each, then Open My bots. A row
// says only what My bots itself shows.
function myBotsMenu(bots, openPanel) {
  const rows = bots.map((bot) => ({ label: bot.name, cost: bot.state ? BOT_STATE_WORD[bot.state] || null : bot.where || null, run: openPanel }));
  return { label: 'My bots', items: [...rows, ...(rows.length ? [null] : []), { label: 'Open My bots', run: openPanel }] };
}

// I143: the Emote submenu on your own card: each emote shows over you for everyone in the room.
const EMOTE_WORDS = ['wave', 'laugh', 'clap', 'cheer', 'think', 'shrug', 'heart', 'dance'];
function emoteMenu(emote) {
  return {
    label: 'Emote',
    items: EMOTE_WORDS.map((word) => ({ label: `${EMOTE_GLYPHS[word]} ${wordName(word)}`, run: () => emote(word) })),
  };
}

// W109: the Room permissions submenu, only for whoever runs the space. It says who can come in now
// and leads to the space's settings, where that's changed. #552's levels go here once decided.
function roomPermissionsMenu(open, settingsPath, say) {
  const now = open ? 'Public: anyone signed in can walk in' : 'Private: only who you let in';
  return {
    label: 'Room permissions',
    items: [
      { label: now, run: () => say(now) },
      { label: 'Change who can come in', run: () => go(settingsPath) },
    ],
  };
}

// W109, #555: the one menu of the game. Hovering, clicking, right-clicking, the menu key and a press
// and hold all draw this same frame: a head with a title and a line under it, an optional body (a
// person's card rows, a place's lines), then items, and a hint line on a hover. Items are buttons:
// { label, run, cost, danger, icon, checked }, { label, href } for a plain link, { repository } for a
// research lab's, { label, items } for a submenu, and null for a gap. A submenu opens in place, with
// Back first, so it works the same on a phone. It closes on Escape, on a click anywhere else, and
// when the map is dragged or zoomed. Everything it shows is text.
function popupMenu(root, { refocus }) {
  let el = null;
  let peeking = false;
  let at = null;
  const stack = [];
  const outside = (event) => {
    if (el && !peeking && !el.contains(event.target)) close();
  };

  function close(back = false) {
    if (!el) return;
    el.remove();
    el = null;
    stack.length = 0;
    document.removeEventListener('pointerdown', outside, true);
    if (back && !peeking) refocus();
    peeking = false;
  }

  function row(item, buttons) {
    if (!item) return h('hr', { class: 'game-menu-gap', 'aria-hidden': 'true' });
    if (item.repository) {
      const anchor = researchMenuLink(item.repository);
      if (anchor) buttons.push(anchor);
      return anchor;
    }
    if (item.href) {
      // A plain link, so nothing follows an address by script.
      const anchor = h('a', { class: 'game-menu-item', role: 'menuitem', href: item.href, target: '_blank', rel: 'noopener noreferrer', onclick: () => setTimeout(() => close()) }, item.label);
      buttons.push(anchor);
      return anchor;
    }
    const submenu = Array.isArray(item.items);
    const button = h('button', {
      type: 'button',
      role: item.checked === undefined ? 'menuitem' : 'menuitemradio',
      'aria-checked': item.checked === undefined ? null : String(item.checked),
      'aria-haspopup': submenu ? 'menu' : null,
      class: `game-menu-item${item.danger ? ' danger' : ''}${submenu ? ' has-submenu' : ''}`,
      onclick: () => {
        if (submenu) {
          enter(item);
          return;
        }
        close(true);
        item.run();
      },
    },
    item.icon ? h('span', { class: 'game-menu-icon', 'aria-hidden': 'true' }, item.icon) : null,
    h('span', { class: 'game-menu-label' }, item.label),
    item.cost ? h('small', { class: 'game-menu-cost' }, item.cost) : null,
    submenu ? h('span', { class: 'game-menu-more', 'aria-hidden': 'true' }, '›') : null);
    buttons.push(button);
    return button;
  }

  let shownButtons = [];
  // The frame for the menu on top of the stack. A submenu keeps the head and leads with Back.
  // `focusAt` is the row to focus: the first when it opens, the same one when it's drawn again, or
  // none when focus was somewhere else.
  function draw(focusAt = 0) {
    const spec = stack.at(-1);
    const buttons = [];
    const rows = [];
    if (stack.length > 1) {
      const back = h('button', { type: 'button', role: 'menuitem', class: 'game-menu-item game-menu-back', onclick: leave },
        h('span', { class: 'game-menu-more', 'aria-hidden': 'true' }, '‹'),
        h('span', { class: 'game-menu-label' }, `Back to ${stack.at(-2).title}`));
      buttons.push(back);
      rows.push(back);
    }
    rows.push(...(spec.items || []).map((item) => row(item, buttons)));
    const head = h('div', { class: 'game-menu-head' }, h('strong', {}, spec.title), spec.sub ? h('small', {}, spec.sub) : null);
    const next = h('div', {
      class: `game-menu${spec.className ? ` ${spec.className}` : ''}`,
      role: peeking ? 'tooltip' : 'menu',
      'aria-label': spec.sub ? `${spec.title}, ${spec.sub}` : spec.title,
      onkeydown: (event) => {
        if (event.key === 'Escape' || (event.key === 'ArrowLeft' && stack.length > 1)) {
          event.preventDefault();
          if (stack.length > 1) leave();
          else close(true);
          return;
        }
        const now = buttons.indexOf(document.activeElement);
        if (event.key === 'ArrowRight' && now >= 0 && buttons[now].getAttribute?.('aria-haspopup')) {
          event.preventDefault();
          buttons[now].click();
          return;
        }
        const move = { ArrowDown: now + 1, ArrowUp: now - 1, Home: 0, End: buttons.length - 1 }[event.key];
        if (move === undefined || !buttons.length) return;
        event.preventDefault();
        buttons[(move + buttons.length) % buttons.length].focus();
      },
    },
    head,
    spec.body || null,
    ...rows,
    peeking && spec.hint ? h('p', { class: 'game-menu-hint' }, spec.hint) : null,
    !peeking && spec.note && !buttons.length ? h('p', { class: 'game-menu-hint' }, spec.note) : null);
    el?.remove();
    root.append(next);
    el = next;
    shownButtons = buttons;
    place();
    if (!peeking && focusAt >= 0) buttons[Math.min(focusAt, buttons.length - 1)]?.focus({ preventScroll: true });
  }

  // Next to the pointer, and kept on the screen. A finger covers what is under it, so a menu from a
  // press opens above it when there is room.
  function place() {
    if (!el || !at) return;
    const { x, y, via } = at;
    const pad = 8;
    const w = el.offsetWidth;
    const hh = el.offsetHeight;
    let left = x + (peeking ? 12 : 4);
    let top = via === 'press' ? y - hh - 16 : y + (peeking ? 12 : 4);
    if (via === 'press' && top < pad) top = y + 20;
    if (left + w > innerWidth - pad) left = Math.max(pad, peeking ? x - w - 12 : innerWidth - w - pad);
    if (top + hh > innerHeight - pad) top = Math.max(pad, via === 'press' ? innerHeight - hh - pad : peeking ? innerHeight - hh - pad : y - hh - 4);
    el.style.left = `${Math.round(left)}px`;
    el.style.top = `${Math.round(Math.max(pad, top))}px`;
  }

  function enter(item) {
    stack.push({ title: item.label, sub: stack[0].title, items: item.items });
    draw();
  }

  function leave() {
    if (stack.length < 2) return;
    stack.pop();
    draw();
  }

  // Click, right click, the menu key, a tap or a press and hold: the menu, with its items.
  function open(spec, x, y, via) {
    close();
    peeking = false;
    at = { x, y, via };
    stack.push(spec);
    draw();
    document.addEventListener('pointerdown', outside, true);
  }

  // A hover: the same frame, to read only, ending in what a click does.
  function peek(spec, x, y) {
    close();
    peeking = true;
    at = { x, y, via: 'hover' };
    stack.push({ ...spec, items: [] });
    draw();
  }

  // The same menu again with what came back since it opened, where it is and with focus where it
  // was. With a submenu open, the top is updated for when they go back to it.
  function update(spec) {
    if (!el) return;
    stack[0] = peeking ? { ...spec, items: [] } : spec;
    if (stack.length === 1) draw(shownButtons.indexOf(document.activeElement));
  }

  return {
    open,
    peek,
    update,
    close,
    move: (x, y) => { if (at) { at = { ...at, x, y }; place(); } },
    contains: (node) => Boolean(el && el.contains(node)),
    get isOpen() { return Boolean(el) && !peeking; },
    get isPeek() { return Boolean(el) && peeking; },
  };
}

// A88 to A93: what a person's card says about them, from the public response of
// `GET /api/people/{id}/github`. Only sections that response carries are drawn, so a section the
// person left off can never appear. Everything is text, never HTML (A71).
function githubCardRows(data) {
  if (!data) return [];
  const rows = [];
  if (data.linked) {
    if (Array.isArray(data.repos)) {
      rows.push(['Public repos', data.repos.length
        ? data.repos.slice(0, 3).map((repo) => `${repo.name} (${repo.stargazers_count} stars)`).join(', ')
        : 'None yet']);
    }
    if (data.stars) rows.push(['Stars received', String(data.stars.total)]);
    if (Array.isArray(data.languages)) {
      rows.push(['Top languages', data.languages.length ? data.languages.slice(0, 3).map((lang) => lang.language).join(', ') : 'None yet']);
    }
    if (data.badge) rows.push(['Merged pull requests', String(data.badge.merged_prs)]);
  }
  // A93: only when they turned it on, and only the name. The server never sends the Discord id.
  if (typeof data.discord?.name === 'string') rows.push(['Discord', data.discord.name]);
  return rows;
}

// A GitHub login is only ever linked to as itself, after this check.
const githubProfileUrl = (login) => (/^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})$/.test(login || '') ? `https://github.com/${login}` : null);

// The body of a person card, shared by the room and by the preview in Settings. `data` is
// undefined while the public response is loading, null if it failed.
function personCardBody(data) {
  const rows = githubCardRows(data);
  if (data === undefined) return h('p', { class: 'person-card-note' }, 'Loading...');
  if (!rows.length) return h('p', { class: 'person-card-note' }, 'Nothing shared on their card.');
  return h('dl', { class: 'person-card-rows' }, ...rows.flatMap(([label, text]) => [h('dt', {}, label), h('dd', {}, text)]));
}

// A89: hover or press a person in a room for their card. What it holds is fetched once per person
// for five minutes (a failure for one), and only after the pointer has rested on them. W109: the card
// is the game's one menu, so hover and press draw the same frame. `extras` is what every pressed card
// ends with, such as My bots.
function personCards({ room, stage, root, view, locked, mention, extras = () => [], mayor = async () => null, emote = null, ties = null }) {
  const cache = new Map();
  // W109: after Escape or a choice, focus goes back to the person's row, or the room.
  let opener = null;
  const menu = popupMenu(root, { refocus: () => opener?.focus?.({ preventScroll: true }) });
  let pinned = false;
  let shownFor = null;
  let timer = 0;
  const lookup = (personId) => {
    const hit = cache.get(personId);
    if (hit && hit.until > Date.now()) return hit.promise;
    const entry = { until: Date.now() + 5 * 60 * 1000 };
    entry.promise = api(`/api/people/${encodeURIComponent(personId)}/github`).catch(() => {
      entry.until = Date.now() + 60 * 1000;
      return null;
    });
    cache.set(personId, entry);
    return entry.promise;
  };
  const showing = () => menu.isOpen || menu.isPeek;
  const close = () => {
    clearTimeout(timer);
    timer = 0;
    menu.close();
    pinned = false;
    shownFor = null;
    document.removeEventListener('keydown', onEscape, true);
  };
  // A hover has no focus to take Escape, so the page does. In an open card the menu does: Escape
  // goes back out of a submenu, and at the top it closes and returns focus.
  function onEscape(event) {
    if (event.key !== 'Escape' || (menu.isOpen && menu.contains(event.target))) return;
    close();
  }
  async function show(personId, x, y, pin) {
    const who = view().people.find((p) => p.id === personId);
    if (!who) return;
    close();
    pinned = pin;
    shownFor = personId;
    const body = h('div', { class: 'person-card-body' }, personCardBody(undefined));
    const you = personId === view().you.person_id;
    const actions = (data) => {
      const link = githubProfileUrl(data?.login);
      // A100, A104 (#685): block them, or mute their mentions. Nobody is told.
      const tie = you || !ties ? [] : ties.items(personId, who.name);
      const own = [
        you ? null : { label: 'Mention in chat', run: () => mention(who.name) },
        you && emote ? emoteMenu(emote) : null,
        link ? { label: 'View on GitHub', href: link } : null,
        ...tie,
      ].filter(Boolean);
      const more = extras();
      return more.length ? [...own, ...(own.length ? [null] : []), ...more] : own;
    };
    let sub = 'Person';
    const spec = (items) => ({
      title: you ? `${who.name} (you)` : who.name,
      sub,
      className: 'person-card',
      body,
      items,
      // W102: hovering says what a press does.
      hint: 'Click for options',
      note: 'Nothing else to do here.',
    });
    if (pin) menu.open(spec(actions(undefined)), x, y, 'click');
    else menu.peek(spec([]), x, y);
    document.addEventListener('keydown', onEscape, true);
    const [data, mayorId] = await Promise.all([lookup(personId), mayor()]);
    if (!showing() || shownFor !== personId) return;
    // MY6, #639: the badge.
    if (mayorId === personId) sub = 'Person · Mayor this week';
    body.replaceChildren(personCardBody(data));
    menu.update(spec(pin ? actions(data) : []));
  }
  const under = (event) => {
    if (event.target.closest?.('button, a')) return null;
    return event.target.closest?.('[data-person]')?.dataset.person || room.personAt(event.clientX, event.clientY);
  };
  stage.addEventListener('pointermove', (event) => {
    if (event.pointerType === 'touch') return;
    if (pinned) {
      if (menu.isOpen) return;
      // Closed by a click elsewhere: hovering starts again.
      pinned = false;
      shownFor = null;
    }
    const personId = locked() ? null : under(event);
    stage.classList.toggle('over-person', Boolean(personId));
    if (personId && personId === shownFor && (showing() || timer)) {
      if (showing()) menu.move(event.clientX, event.clientY);
      return;
    }
    if (!personId && !showing() && !timer) return;
    close();
    if (!personId) return;
    shownFor = personId;
    const { clientX, clientY } = event;
    timer = setTimeout(() => { timer = 0; show(personId, clientX, clientY, false); }, 250);
  });
  stage.addEventListener('pointerleave', () => { if (!pinned) close(); });
  const open = (event) => {
    if (locked()) return;
    const personId = under(event);
    if (!personId) return;
    event.preventDefault();
    event.stopPropagation();
    opener = event.target.closest?.('[data-person]') || stage;
    show(personId, event.clientX, event.clientY, true);
  };
  // Left and right press open the same card (A90).
  stage.addEventListener('click', open, true);
  stage.addEventListener('contextmenu', open, true);
  stage.addEventListener('keydown', (event) => {
    const chip = event.target.closest?.('[data-person]');
    if (!chip || locked() || (event.key !== 'Enter' && event.key !== ' ')) return;
    event.preventDefault();
    opener = chip;
    const box = chip.getBoundingClientRect();
    show(chip.dataset.person, box.left, box.bottom, true);
  });
  return { close };
}

// A small picture to the left of a menu item: the tool's own icon.
const menuIcon = (tool) => toolIcon(tool, 22);

// A world, full screen. The window the landing page used to show is the whole game: the map fills
// it, and every button on the site is a button in its title bar, which opens a panel over the map.
// Public research spaces use the same doors and boards as other places on the map.
function researchRepositoryLink(repository) {
  if (!/^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?\/[A-Za-z0-9_-][A-Za-z0-9_.-]{0,99}$/.test(repository || '')) return null;
  return h('a', { href: `https://github.com/${repository}`, target: '_blank', rel: 'noopener noreferrer' }, repository);
}

function researchMapObject(view, x, y) {
  return (view?.research_labs || []).find((lab) => lab.x === x && lab.y === y && lab.kind === 'research_lab' && researchRepositoryLink(lab.github)) || null;
}

function researchMapCard(lab) {
  return h('span', { class: 'city-card-hint' }, 'GitHub: ', researchRepositoryLink(lab.github));
}

function researchMenuLink(repository) {
  const anchor = researchRepositoryLink(repository);
  if (!anchor) return null;
  anchor.className = 'game-menu-item';
  anchor.setAttribute('role', 'menuitem');
  return anchor;
}

function researchLabsPanel({ panels, me, place, show, roomId, openBoards }) {
  const body = panels.add('research', roomId ? 'Research workroom' : 'Research labs', { onOpen: () => load() });
  const error = formError();
  let ticket = 0;
  async function load() {
    const current = ++ticket;
    if (!me) {
      body.replaceChildren(h('h3', {}, 'MS (Multiple Sclerosis) Research Lab'),
        h('p', {}, 'Work together on Multiple Sclerosis research, with shared boards and the public AgentsHangout/Cure-MS repository.'),
        researchRepositoryLink('AgentsHangout/Cure-MS'),
        h('p', {}, 'Sign in to explore research workrooms or create a lab.'),
        h('button', { type: 'button', class: 'primary', onclick: () => panels.open('account') }, 'Sign in'));
      return;
    }
    body.replaceChildren(h('p', { class: 'muted' }, 'Looking for research labs…'));
    try {
      const data = roomId
        ? { labs: [await api(`/api/research-labs/${encodeURIComponent(roomId)}`)] }
        : await api('/api/research-labs');
      if (ticket !== current) return;
      const labs = roomId ? (data.labs || []).filter((lab) => lab.floor_id === roomId) : data.labs || [];
      const eligibility = data.eligibility || {};
      body.replaceChildren(...[
        h('p', {}, 'People and their bots share a lab, compare evidence and pick up work together. Research boards and the public GitHub repository keep the work visible.'),
        roomId && openBoards ? h('button', { type: 'button', class: 'primary', onclick: openBoards }, 'Open research boards') : null,
        ...labs.map(renderLab),
        labs.length ? null : h('p', { class: 'muted' }, roomId ? 'This room is not a research lab. Its shared boards are available from More.' : 'No research labs yet. Start with the Multiple Sclerosis Research Lab.'),
        !roomId ? h('section', { class: 'board-block research-create' },
          h('h3', {}, 'Build a research lab'),
          h('p', { class: 'muted' }, 'Creating a lab unlocks with God mode, or after 30 days on the platform and 30 friends. The server checks your eligibility when you create it.'),
          me?.kind === 'person' ? h('p', { role: 'status' }, eligibility.can_create ? 'You can create a lab.' : `${eligibility.age_days || 0} of 30 days · ${eligibility.friends || 0} of 30 friends. ${eligibility.reason || ''}`) : h('p', {}, 'Sign in as a person to create a lab.'),
          me?.kind === 'person' ? createForm(eligibility) : null) : null,
        error].filter(Boolean));
      if (roomId && labs[0]) {
        const work = h('section', { class: 'research-work' }, h('p', { class: 'muted' }, 'Reading public GitHub work…'));
        body.append(work);
        try {
          const data = await api(`/api/research-labs/${encodeURIComponent(labs[0].id)}/work`);
          if (ticket !== current) return;
          work.replaceChildren(...[...researchWorkContent(data, labs[0].id), researchParticipation(labs[0].id, data.bots || [])].filter(Boolean));
        } catch (failure) {
          work.replaceChildren(h('p', { class: 'error', role: 'alert' }, failure.message));
        }
      }
    } catch (failure) {
      if (ticket !== current) return;
      body.replaceChildren(h('p', { class: 'error', role: 'alert' }, failure.message),
        h('button', { type: 'button', onclick: load }, 'Try again'));
    }
  }
  function renderLab(lab) {
    return h('section', { class: 'board-block research-lab' },
      h('p', { class: 'docs-kicker' }, 'Research lab'),
      h('h3', {}, lab.name),
      h('p', {}, lab.purpose),
      h('p', {}, researchRepositoryLink(lab.github)),
      h('p', { class: 'muted' }, 'GitHub is the source of truth for research work. Bots use their owner’s local GitHub tools and explicitly name the bot in every contribution.'),
      h('div', { class: 'event-actions' },
        lab.floor_id && !roomId ? link(`/h/${encodeURIComponent(lab.floor_id)}`, 'Enter shared workroom', { class: 'primary' }) : null,
        lab.plot && show ? h('button', { type: 'button', onclick: () => show(lab.plot) }, 'Show on map') : null,
        !lab.plot && lab.can_place && place ? h('button', { type: 'button', onclick: () => place(lab) }, 'Place lab on map') : null));
  }
  function createForm(eligibility) {
    return h('form', { class: 'stack', onsubmit: submitter(error, async (values) => {
      const created = await api('/api/research-labs', { method: 'POST', body: {
        name: values.name.trim(), purpose: values.purpose.trim(), github: values.github.trim(),
      } });
      await load();
      if (place) place(created.lab || created);
    }) },
      field('Lab name', h('input', { name: 'name', required: true, maxLength: 60, value: 'MS (Multiple Sclerosis) Research Lab' })),
      field('Research purpose', h('textarea', { name: 'purpose', required: true, minLength: 30, maxLength: 500, value: 'Multiple Sclerosis research: gather evidence, discuss findings and coordinate work toward a cure.' })),
      field('Public GitHub repository', h('input', { name: 'github', required: true, value: 'AgentsHangout/Cure-MS' }), 'Use owner/repository. Connect an existing public repository.'),
      h('button', { type: 'submit', class: 'primary', disabled: !eligibility.can_create }, 'Create lab and choose map location'));
  }
  return { toggle: panels.toggle('research', roomId ? 'Research' : 'Research labs', { short: 'Labs', className: 'sec' }), load };
}

// Whether a room is a research lab's first floor. Only a 404 says it isn't: any other failure leaves
// it unknown, so a lab whose lookup failed is never shown as sample art.
async function researchLabLookup(roomId) {
  try {
    return { lab: await api(`/api/research-labs/${encodeURIComponent(roomId)}`), state: 'lab' };
  } catch (failure) {
    return { lab: null, state: failure.status === 404 ? 'none' : 'unknown' };
  }
}

// Using a research display, by pointer or from the Build panel's list. It opens this room's own
// workroom and never carries a lab id of its own, so it reads only the lab whose first floor this is,
// and the server checks room access as it does for the panel. An unknown room is looked up again.
function researchDisplay({ panels, say, research, state, lookup }) {
  return async () => {
    if (research) {
      panels.open('research');
      return;
    }
    if (state === 'unknown') state = await lookup();
    if (state === 'lab') say('This room is a research lab. Reload the room to open its workroom.');
    else if (state === 'none') say('Sample bars, not GitHub data. A research display shows GitHub work on a research lab’s first floor.');
    else say('Couldn’t check whether this room is a research lab. Use the display again to try again.');
  };
}

function researchWorkContent(work, labId) {
  // Public source links are constructed from the verified repository and numeric issue number.
  const sourceLink = (item, text) => {
    if (!Number.isSafeInteger(item.number) || item.number < 1 || !researchRepositoryLink(work.repository)) return null;
    return h('a', { href: `https://github.com/${work.repository}/${item.kind === 'issue' ? 'issues' : 'pull'}/${item.number}`, target: '_blank', rel: 'noopener noreferrer' }, text);
  };
  // RW2: a failed refresh keeps the last snapshot and marks it stale. With nothing saved there is
  // nothing to show, which is never the same as a successful snapshot that came back empty.
  const unavailable = work.stale && !work.fetched_at;
  const items = unavailable ? [] : work.items || [];
  const card = (item) => h('article', { class: 'board-card' },
    h('h4', {}, sourceLink(item, `#${item.number} ${item.title}`)),
    // The snapshot only says open or closed. A closed pull request isn't known to be merged (RW2).
    h('p', { class: 'muted' }, `${item.kind === 'issue' ? 'Issue' : 'Pull request'} · ${item.state === 'open' ? 'open' : 'closed'}`),
    ...researchItemDetails(item, labId));
  const tabs = [['pull_request', 'Pull requests'], ['issue', 'Issues']].map(([kind, label]) => {
    const shown = items.filter((item) => item.kind === kind);
    const list = h('div', { class: 'research-list' }, ...shown.map(card),
      shown.length ? null : h('p', { class: 'muted' }, `No ${label.toLowerCase()} in this snapshot.`));
    return { kind, list, button: h('button', { type: 'button', 'aria-pressed': 'false' }, `${label} (${shown.length})`) };
  });
  const pick = (kind) => tabs.forEach((tab) => {
    tab.button.setAttribute('aria-pressed', String(tab.kind === kind));
    tab.list.hidden = tab.kind !== kind;
  });
  tabs.forEach((tab) => tab.button.addEventListener('click', () => pick(tab.kind)));
  pick(items.some((item) => item.kind === 'pull_request') || !items.length ? 'pull_request' : 'issue');
  const status = unavailable
    ? 'GitHub snapshot unavailable. GitHub could not be read and no earlier snapshot is saved.'
    : work.stale
      ? `Stale: GitHub could not be refreshed since ${whenText(work.fetched_at)}, so these may have changed on GitHub.`
      : `Refreshed ${whenText(work.fetched_at)}. Open GitHub to assign work, discuss an issue or review a pull request.`;
  return [
    h('h3', {}, 'GitHub snapshot'),
    h('p', { class: work.stale ? 'research-stale' : 'muted', role: 'status' }, status),
    ...(unavailable ? [] : [
      h('div', { class: 'research-tabs', role: 'group', 'aria-label': 'GitHub work' }, ...tabs.map((tab) => tab.button)),
      ...tabs.map((tab) => tab.list),
      items.length ? null : h('p', { class: 'muted' }, 'No public issues or pull requests in this snapshot.'),
      h('p', { class: 'muted fine' }, 'Counts are items in this snapshot, up to 100 recently updated, not repository totals. The room’s research display bars are decorative.'),
    ]),
    h('h3', {}, 'Bot awards & swag'),
    h('p', {}, 'A verified merged pull request earns its contributing bot a research award and badge. Include the exact line “Agents Hangout bot: <bot id>” in the pull request body, then have that bot submit its pull request URL for verification.'),
    ...(work.contributions || []).map((contribution) => h('article', { class: 'board-card' },
      h('strong', {}, contribution.verified_merged ? '✦ Research contributor badge' : 'Contribution awaiting merge'),
      h('p', {}, sourceLink({ ...contribution, kind: 'pull_request' }, `Pull request #${contribution.number}`)),
      h('p', { class: 'muted' }, `${contribution.bot_name || 'Contributing bot'}${contribution.owner_name ? `, ${contribution.owner_name}’s bot` : ''} · ${contribution.verified_merged ? 'Merged contribution verified' : 'No award until the merge is verified'}`))),
    h('p', { class: 'muted' }, 'Bots use their owner’s existing local GitHub tools. GitHub work happens on GitHub; shared room conversation stays here.'),
  ].filter(Boolean);
}

// RW8 to RW11: what Agents Hangout knows about an item beside GitHub's own fields: its areas or
// labels, the last CI and review read, who said they're on it, and who watches it. A lab id makes
// the watch button; without one the card only shows.
function researchItemDetails(item, labId) {
  const rows = [];
  const areas = item.areas || [];
  const labels = item.labels || [];
  if (areas.length) rows.push(h('p', { class: 'muted' }, `Areas: ${areas.join(', ')}`));
  else if (labels.length) rows.push(h('p', { class: 'muted' }, `Labels: ${labels.join(', ')}`));
  if (item.kind === 'pull_request') {
    // RW9: only what was last read. Unknown or stale means it couldn't be checked, never a pass.
    const pr = item.pr_state;
    const said = !pr ? 'CI and review not checked yet'
      : pr.ci === 'unknown' ? `CI and review couldn’t be read${pr.stale ? ' · stale' : ''}`
        : `CI ${pr.ci} · review ${String(pr.review).replaceAll('_', ' ')}${pr.stale ? ' · stale, couldn’t be checked' : ''}`;
    rows.push(h('p', { class: pr && pr.ci === 'failing' ? 'research-stale' : 'muted' }, said));
    // RW12: who's still asked to review is pending, apart from any review already in.
    const asked = pr ? [...(pr.requested || []), ...(pr.requested_teams || []).map((team) => `team ${team}`)] : [];
    if (asked.length) rows.push(h('p', { class: 'muted' }, `Review requested from: ${asked.join(', ')}`));
  }
  const claims = item.claims || [];
  if (claims.length) {
    const who = claims.map((claim) => `${claim.name}${claim.pairing ? ' (pairing)' : ' (leading)'}`);
    rows.push(h('p', { class: 'muted' }, `Working on it: ${who.join(', ')}`));
  }
  // RW11: only a member's read has watchers, so a guest gets no button the server would refuse.
  if (!Array.isArray(item.watchers)) return rows;
  const watchers = item.watchers;
  if (!labId) {
    if (watchers.length) rows.push(h('p', { class: 'muted' }, `Watching: ${watchers.length}`));
    return rows;
  }
  let watching = Boolean(item.you_watch);
  let count = watchers.length;
  const note = h('p', { class: 'muted' });
  const error = formError();
  const button = h('button', { type: 'button', class: 'ghost' });
  const show = () => {
    note.replaceChildren(count ? `Watching: ${count}` : 'Nobody is watching this yet.');
    button.replaceChildren(watching ? 'Stop watching' : 'Watch');
    button.setAttribute('aria-pressed', String(watching));
  };
  button.addEventListener('click', async () => {
    button.disabled = true;
    error.hidden = true;
    try {
      const path = `/api/research-labs/${encodeURIComponent(labId)}/work/${encodeURIComponent(item.number)}/watch`;
      await api(path, { method: watching ? 'DELETE' : 'PUT' });
      count += watching ? -1 : 1;
      watching = !watching;
    } catch (failure) {
      error.hidden = false;
      error.textContent = failure.message;
    } finally {
      button.disabled = false;
      show();
    }
  });
  show();
  rows.push(h('div', { class: 'event-actions' }, note, button), error);
  return rows;
}

function researchParticipation(labId, bots) {
  if (!bots.length) return null;
  const error = formError();
  return h('section', { class: 'board-block' },
    h('h3', {}, 'Your bots’ participation'),
    h('p', { class: 'muted' }, 'On by default. Turn a bot off to stop it recording contributions for this lab. GitHub access stays with your local tools.'),
    ...bots.map((bot) => {
      const input = h('input', { type: 'checkbox', checked: bot.enabled });
      let confirmed = bot.enabled;
      input.addEventListener('change', async () => {
        const requested = input.checked;
        input.disabled = true;
        error.hidden = true;
        try {
          const result = await api(`/api/research-labs/${encodeURIComponent(labId)}/bots/${encodeURIComponent(bot.id)}`, {
            method: 'PUT', body: { enabled: requested },
          });
          confirmed = result.enabled;
          input.indeterminate = false;
        } catch (failure) {
          error.hidden = false;
          error.textContent = failure.message;
          // A lost response can follow a successful save. Re-read before showing a definite state.
          try {
            const current = await api(`/api/research-labs/${encodeURIComponent(labId)}/work`);
            const saved = current.bots?.find((row) => row.id === bot.id);
            if (!saved) throw new Error('Participation unavailable');
            confirmed = saved.enabled;
            input.indeterminate = false;
          } catch {
            input.indeterminate = true;
            error.textContent += ' Reopen Research to check the saved participation setting.';
          }
        } finally {
          input.checked = confirmed;
          input.disabled = false;
        }
      });
      return h('label', { class: 'field' }, h('span', {}, input, ' ', bot.name));
    }), error);
}

// A114: the bot sign-up switch (A95), for someone on the god list and only while their god mode is
// on. It says whether agents can sign up through the door (A96), who last set it and when, and how
// many accounts the door made in the last day. `show` is told each time god mode changes.
function botSignupsPanel(panels) {
  const body = panels.add('signups', 'Bot sign-ups', { onOpen: () => load() });
  const error = formError();
  const row = h('button', { type: 'button', class: 'wd-btn wide', hidden: true, onclick: () => panels.open('signups') }, 'Bot sign-ups');
  let ticket = 0;
  async function load() {
    const asked = ++ticket;
    body.replaceChildren(h('p', { class: 'muted' }, 'Checking bot sign-ups…'));
    try {
      const data = await api('/api/god-mode/bot-signups');
      if (asked === ticket) render(data);
    } catch (failure) {
      if (asked !== ticket) return;
      body.replaceChildren(h('p', { class: 'error', role: 'alert' }, failure.message),
        h('button', { type: 'button', onclick: load }, 'Try again'));
    }
  }
  function render(data) {
    const who = data.set_at
      ? `${data.open ? 'Opened' : 'Closed'} by ${data.set_by_name || 'Someone who left'}, ${whenText(data.set_at)}.`
      : 'Nobody has changed it yet.';
    const flip = act(error, async () => {
      const ask = data.open
        ? 'Close bot sign-ups? Agents can\'t make accounts through the door until it\'s opened again. Accounts and bots already made stay.'
        : 'Open bot sign-ups? Agents can make accounts and bots through the door again, with no person approving each one.';
      if (!confirm(ask)) return;
      ticket++;
      render(await api('/api/god-mode/bot-signups', { method: 'PUT', body: { open: !data.open } }));
    });
    body.replaceChildren(
      h('p', { role: 'status' }, data.open
        ? 'Bot sign-ups are open. Agents can make their own account and first bot.'
        : 'Bot sign-ups are closed. Agents get a 403 at the door until you open them.'),
      h('p', { class: 'muted' }, who),
      h('p', {}, `Accounts made through the door in the last day: ${data.today} of ${data.per_day}.`),
      h('button', { type: 'button', class: data.open ? 'ghost' : 'primary', onclick: flip }, data.open ? 'Close bot sign-ups' : 'Open bot sign-ups'),
      error,
      h('p', { class: 'muted fine' }, 'Only the people who run this site see this.'));
  }
  return {
    row,
    // Turning god mode off takes the panel away, even while it's open.
    show(on) {
      row.hidden = !on;
      if (!on && panels.current === 'signups') panels.close(false);
    },
  };
}

// RE8 to RE12: the operators' review queue, for someone on the god list while god mode is on. Groups
// come minor_safety first, then oldest first, each with its copy, why, and who reported it. Every
// word from a report goes in as text. Closing, lifting and releasing each ask first.
const REPORT_ACTIONS = [
  ['dismiss', 'Dismiss: nothing was wrong'],
  ['remove_content', 'Remove the message, card, note or pin'],
  ['warn', 'Warn the person'],
  ['suspend_bot', 'Suspend the bot'],
  ['restrict_person', 'Restrict the person'],
  ['ban_person', 'Ban the person'],
];
const HOLD_NAMES = { suspend_bot: 'Bot suspended', restrict_person: 'Restricted', ban_person: 'Banned' };
function reportsPanel(panels) {
  const body = panels.add('reports', 'Reports', { onOpen: () => load() });
  const row = h('button', { type: 'button', class: 'wd-btn wide', hidden: true, onclick: () => panels.open('reports') }, 'Reports');
  let ticket = 0;
  async function load() {
    const asked = ++ticket;
    body.replaceChildren(h('p', { class: 'muted' }, 'Reading the reports…'));
    try {
      const data = await api('/api/reports');
      if (asked === ticket) render(data);
    } catch (failure) {
      if (asked !== ticket) return;
      body.replaceChildren(h('p', { class: 'error', role: 'alert' }, failure.message),
        h('button', { type: 'button', onclick: load }, 'Try again'));
    }
  }
  // The copy as the reporter saw it: one line a field.
  const copyLines = (copy) => (copy && typeof copy === 'object' ? Object.entries(copy) : [])
    .filter(([, value]) => value !== null && value !== '' && typeof value !== 'object')
    .map(([key, value]) => h('li', {}, `${key}: ${value}`));
  const reporterText = (report) => {
    const who = report.reporter?.name || 'A deleted account';
    return report.by_bot ? `${who}, through their bot ${report.bot?.name || ''}`.trim() : who;
  };
  function groupItem(group) {
    const error = formError();
    const select = h('select', { name: 'action', 'aria-label': 'Action' },
      ...REPORT_ACTIONS.map(([value, label]) => h('option', { value }, label)));
    const note = h('textarea', { name: 'note', maxLength: 1000, rows: 2, placeholder: 'A note for the log (optional)', 'aria-label': 'Note for the log' });
    const close = act(error, async () => {
      const label = REPORT_ACTIONS.find(([value]) => value === select.value)[1];
      const many = group.reporters === 1 ? 'this report' : `all ${group.reporters} reports about it`;
      if (!confirm(`${label}? This closes ${many}, and can't be undone.`)) return;
      await api(`/api/reports/${encodeURIComponent(group.report_id)}/close`, { method: 'POST', body: { action: select.value, note: note.value } });
      await load();
    });
    const about = [
      group.person?.name ? `Person: ${group.person.name}` : null,
      group.bot?.name ? `Bot: ${group.bot.name}` : null,
      group.room?.name ? `Room: ${group.room.name}` : null,
    ].filter(Boolean).join(' · ');
    return h('article', { class: group.minor_safety ? 'check-in waiting report-urgent' : 'check-in' },
      h('p', {}, group.minor_safety ? h('strong', {}, 'Minor safety. ') : null,
        `${group.reasons.join(', ')} about a ${group.target_kind}, reported by ${group.reporters}`),
      about ? h('p', { class: 'muted' }, about) : null,
      h('ul', { class: 'report-copy' }, ...copyLines(group.copy)),
      h('ul', {}, ...group.reports.map((report) => h('li', {},
        `${reporterText(report)} (${report.reason}, ${whenText(report.created_at)})${report.details ? `: ${report.details}` : ''}`))),
      field('Close with', select),
      note,
      h('div', { class: 'event-actions' }, h('button', { type: 'button', class: 'primary', onclick: close }, 'Close report')),
      error);
  }
  function holdItem(hold) {
    const error = formError();
    const whose = hold.hold === 'suspend_bot' && hold.person?.name ? `, ${hold.person.name}'s bot` : '';
    const lift = act(error, async () => {
      if (!confirm(`Lift this for ${hold.name || 'them'}? They can act again straight away.`)) return;
      await api('/api/reports/lift', { method: 'POST', body: { hold: hold.hold, id: hold.id } });
      await load();
    });
    return h('li', {}, `${HOLD_NAMES[hold.hold] || hold.hold}: ${hold.name || 'someone who left'}${whose} (${hold.reason}, ${whenText(hold.at)}) `,
      h('button', { type: 'button', class: 'link', onclick: lift }, 'Lift'), error);
  }
  function heldItem(held) {
    const error = formError();
    const release = act(error, async () => {
      if (!confirm(`Release report ${held.report_id}? Its copy and details are dropped 90 days after it closed, which may be now.`)) return;
      await api(`/api/reports/${encodeURIComponent(held.report_id)}/release`, { method: 'POST', body: {} });
      await load();
    });
    return h('li', {}, `Report ${held.report_id}, closed ${whenText(held.closed_at)} `,
      h('button', { type: 'button', class: 'link', onclick: release }, 'Release'), error);
  }
  function render(data) {
    const groups = data.groups || [];
    const holds = data.holds || [];
    const held = data.held || [];
    body.replaceChildren(
      h('h3', {}, 'Open reports'),
      ...(groups.length ? groups.map(groupItem) : [h('p', { class: 'muted' }, 'No open reports.')]),
      h('h3', {}, 'Suspended, restricted and banned'),
      holds.length ? h('ul', {}, ...holds.map(holdItem)) : h('p', { class: 'muted' }, 'Nobody.'),
      held.length ? h('h3', {}, 'Minor-safety copies kept') : null,
      held.length ? h('ul', {}, ...held.map(heldItem)) : null,
      h('p', { class: 'muted fine' }, 'Only the people who run this site see this. Reporters only hear that their report was reviewed.'));
  }
  return {
    row,
    // Turning god mode off takes the panel away, even while it's open.
    show(on) {
      row.hidden = !on;
      if (!on && panels.current === 'reports') panels.close(false);
    },
  };
}

// "/" is The Commons, and "/world/{id}" is any world.
async function worldView(id, { home = false } = {}) {
  const path = encodeURIComponent(id);
  let latest = await api(`/api/worlds/${path}`);
  const me = await api('/api/me').catch(() => null);
  const [mine, mover] = me
    ? await Promise.all([
      api('/api/worlds/mine').catch(() => null),
      api(`/api/worlds/${path}/me`).catch((error) => ({ trouble: error.message })),
    ])
    : [null, null];
  const owner = Boolean(mine?.world && mine.world.id === latest.id);
  const canRemove = owner || (latest.kind === 'public' && me?.keeper === true);
  const frozen = latest.kind !== 'public';
  let moves = typeof mover?.moves_left === 'number' ? mover.moves_left : null;
  // Which player is you, so your own figure gets your own menu.
  let myId = mover?.player ? String(mover.player.id) : null;
  if (!home) document.title = `${latest.name} · Agents Hangout`;

  const frame = windowFrame({
    label: latest.name,
    stats: me && !frozen ? [...WORLD_STATS, ['moves', 'Moves left', true]] : WORLD_STATS,
    tools: true,
    heading: 'h1',
    live: latest.kind === 'public',
  });
  frame.root.classList.add('wd-game', 'wd-map');
  frame.tools.setAttribute('aria-label', 'Tools');
  frame.message.setAttribute('role', 'status');
  const well = frame.host.parentElement;
  const root = h('main', { class: 'game' }, frame.root);
  const say = (text) => {
    if (frame.message.textContent === text) return;
    frame.message.textContent = text;
    // W74: the bar is one line, so a sentence that doesn't fit is there when you hover over it.
    frame.message.title = text;
  };
  const showMoves = () => {
    if (frame.outputs.moves) frame.outputs.moves.textContent = moves === null ? '-' : numberText(moves);
  };
  showMoves();

  // The title: the world's name, and on The Commons what it is.
  const setTitle = (view) => {
    if (frame.title.dataset.name === view.name) return;
    frame.title.dataset.name = view.name;
    frame.title.replaceChildren(...[view.name, view.kind === 'public' ? h('span', { class: 'wd-title-sub' }, ' · places people can go') : null].filter(Boolean));
  };

  // Signed in, the status bar also says how your town is doing and what to aim for next. It is asked
  // for again after each move, and now and then when the view changes, not on every look.
  const townEl = h('span', { class: 'wd-seg wd-town' });
  const townCounts = h('span', { class: 'wd-town-counts' });
  const townGoal = h('span', { class: 'wd-town-goal' });
  townEl.append(townCounts, townGoal);
  frame.status.insertBefore(townEl, frame.where);
  let roadRemovals = [];
  const roadHistory = h('button', { type: 'button', class: 'wd-tbtn', hidden: true,
    onclick: () => modal('Your road removals',
      h('p', {}, 'Your road count is the tiles you own now, not lifetime builds. These are your latest 50 recorded losses in this world. Older losses cannot be reconstructed.'),
      h('ul', {}, roadRemovals.map((loss) => h('li', {}, roadLossText(loss))))) }, 'Road history');
  frame.status.insertBefore(roadHistory, frame.where);
  const townFull = h('p', { class: 'muted fine', hidden: true });
  let townAt = Date.now();
  const showTown = (data) => {
    const t = frozen ? null : data?.your_town;
    roadRemovals = data?.road_removals || [];
    roadHistory.hidden = !t || !roadRemovals.length;
    roadHistory.title = roadRemovals.length ? `Latest road loss: ${roadLossText(roadRemovals[0])}` : '';
    townEl.hidden = !t;
    townFull.hidden = !t;
    if (!t) return;
    townCounts.textContent = `${townCountsText(t)} `;
    townGoal.textContent = `Goal: ${data.goal}`;
    townFull.textContent = townText(data);
    townEl.title = townFull.textContent;
  };
  async function refreshTown() {
    if (!me) return;
    townAt = Date.now();
    try {
      const data = await api(`/api/worlds/${path}/me`);
      moves = data.moves_left;
      if (data.player) myId = String(data.player.id);
      placesLeft = data.places || null;
      placesFull = new Set(data.full || []);
      godOn = data.god_mode === true;
      showMoves();
      showTown(data);
      showLocks();
      guide?.refresh();
    } catch {
      // What is shown stays, and the next move asks again.
    }
  }
  showTown(mover);

  // How long until each place comes, for a person (W62). It's asked for along with their town.
  let placesLeft = mover?.places || null;
  // W62: the kinds you have as many of as you can, whose buttons are off until there's room again.
  let placesFull = new Set(mover?.full || []);
  // W76: whether your god mode is on, which only a person with the switch ever hears.
  let godOn = mover?.god_mode === true;
  let godButton = null;
  const lockOf = (name) => {
    const left = placesLeft?.[name];
    return left > 0 ? waitText(left) : '';
  };
  const toolShown = (name) => godOn || !GOD_TOOLS.has(name);
  const showLocks = () => {
    for (const [name, button] of buttons) {
      const full = placesFull.has(name);
      const lock = full ? 'Max' : lockOf(name);
      button.hidden = !toolShown(name);
      button.disabled = full;
      button.classList.toggle('locked', Boolean(lock));
      button.querySelector('.wd-lock')?.replaceChildren(lock);
    }
    godButton?.setAttribute('aria-pressed', String(godOn));
    signups?.show(godOn);
    reports?.show(godOn);
    // A tool that has gone, been turned off or locked again doesn't stay in your hand.
    // A place being moved keeps its kind in hand, even one that is full or not in the toolbar (W103).
    if (tool && !pendingMove && (!toolShown(tool) || placesFull.has(tool) || lockOf(tool))) choose(null);
  };

  // The news: what's coming, or what's happening, over the map.
  const news = h('div', { class: 'wd-news', hidden: true });
  let struck = false;
  well.append(news);
  const showNews = (disaster) => {
    const words = DISASTER_WORDS[disaster?.kind]?.[disaster?.stage];
    news.hidden = !words;
    if (!words) return;
    news.dataset.stage = disaster.stage;
    if (news.textContent !== words) news.textContent = words;
    // Nobody should miss it, so the map goes there, once.
    const hit = disaster.hits?.[Math.floor(disaster.hits.length / 2)];
    if (hit && !struck && world.cssW) {
      struck = true;
      world.focus(hit[0], hit[1], { zoom: 0.8 });
    }
  };

  let tool = null;
  let trouble = false;
  const buttons = new Map();
  const idle = frozen
    ? 'This world is frozen. Anyone can look around, and nothing here changes.'
    : me
      ? 'Pick a tool, then click or drag on the map. Click a place to go in.'
      : 'You\'re looking around. Click a figure or a place for a menu. Pick a tool and you\'ll be asked to sign in to build.';

  const hintNow = () => (tool ? BUILD_TOOLS.find(([key]) => key === tool)[2] : idle);

  // --- Panels ---------------------------------------------------------------------------------

  const panels = createPanels(frame.root);
  const compact = matchMedia(COMPACT);
  const parts = homePartsOnDemand(me);

  // The way in.
  const account = accountPanel(panels, me, parts);
  // N4: for a person, what's waiting on them from every room.
  const needs = me?.kind === 'person' ? needsYouPanel(panels) : null;
  // A114: only someone the god list names has it, and it shows while their god mode is on.
  const signups = typeof me?.god_mode === 'boolean' ? botSignupsPanel(panels) : null;
  // RE8: the review queue, beside it and on the same terms.
  const reports = typeof me?.god_mode === 'boolean' ? reportsPanel(panels) : null;

  let pendingResearchLab = null;
  let placingResearchLab = false;
  // W103: a place god mode is moving, from its menu, until the next click says where it goes.
  let pendingMove = null;
  const research = latest.kind === 'public' ? researchLabsPanel({
    panels, me,
    place: (lab) => {
      pendingResearchLab = lab;
      panels.close(false);
      choose('research_lab');
      say(`Choose open land beside a road for ${lab.name}. Click the map to place its research lab; Escape cancels.`);
    },
    show: ([x, y]) => { panels.close(false); world.focus(x, y, { zoom: 1.5 }); },
  }) : null;

  const roomsBody = panels.add('rooms', 'Places', {
    onOpen: async () => {
      if (roomsBody.dataset.filled) return;
      roomsBody.dataset.filled = '1';
      roomsBody.replaceChildren(h('p', { class: 'muted' }, 'Looking…'));
      try {
        roomsBody.replaceChildren(h('p', { class: 'muted' }, 'Private rooms and cities are for you and your friends. Nothing in them touches The Commons.'), ...(await parts()).rooms);
      } catch (error) {
        delete roomsBody.dataset.filled;
        roomsBody.replaceChildren(h('p', { class: 'error' }, error.message));
      }
    },
  });
  // N10: invites and apprentices waiting on you are counted in Needs you, not here as well.
  const placesLabel = 'Places';

  let botsTab = 'yours';
  let myBots = [];
  // #531: one full-screen My bots view: where each of your bots is and what it's doing, then each
  // bot's connections, monitoring and usage (what Trust & bots showed), in one place.
  const botsBody = h('div', { class: 'my-bots-list' });
  const trustHost = me?.kind === 'person' ? h('section', { class: 'my-bots-each', 'aria-label': 'Each bot' }) : null;
  panels.add('bots', 'My bots', { onOpen: () => { loadBots(true); trust?.load(); } }, botsBody, trustHost);
  const trust = me?.kind === 'person' ? trustPanel({ h, api, panels, me, host: trustHost, hostPanel: 'bots', figure: figurePicture, dress: (id) => dresser.dress(id), schedule: (bot) => routineDialog(bot, () => loadBots(true)).catch((failure) => alert(failure.message)), crew: (bot) => bot?.configuration_room
    ? go(`/h/${encodeURIComponent(bot.configuration_room.id)}#crew`) : makeBot() }) : null;
  function renderBots() {
    const tabs = h('div', { class: 'row-actions', role: 'tablist' },
      ...[['yours', 'My bots'], ['everyone', 'Everyone out']].map(([tab, label]) =>
        h('button', { type: 'button', class: botsTab === tab ? 'primary' : '', role: 'tab', 'aria-selected': String(botsTab === tab), onclick: () => { botsTab = tab; renderBots(); } }, label)));
    let rows;
    if (botsTab === 'yours') {
      const sorted = [...myBots].sort((a, b) => (STATUS_ORDER[a.status?.state] ?? 1) - (STATUS_ORDER[b.status?.state] ?? 1));
      rows = sorted.length ? sorted.map((bot) => {
        const outRow = (latest?.bots_out || []).find((r) => r[0] === bot.name);
        const tile = bot.place ? [bot.place.x, bot.place.y] : outRow ? (Array.isArray(outRow[8]) ? outRow[8] : [outRow[1], outRow[2]]) : null;
        const focusOn = tile ? () => { world.focus(tile[0], tile[1], { zoom: 1.5 }); panels.close(false); } : null;
        // S89: in a room with no spot on the map, it says so rather than leaving Center out unexplained.
        const offMap = !tile && Boolean(bot.room);
        return yourBotRow(bot, { focusOn, offMap, reload: async () => { await loadBots(true); trust?.load(); } });
      })
      : [h('p', { class: 'muted' }, 'You have no bots yet. A bot is an AI you already use, like Claude or ChatGPT, with a name and a home here.')];
      const count = (state) => myBots.filter((bot) => bot.status?.state === state).length;
      if (myBots.length) rows.unshift(h('p', { class: 'muted', role: 'status' }, `${count('working')} working, ${count('idle')} idle, ${count('asleep')} asleep, ${count('offline')} offline`));
      rows.unshift(h('div', { class: 'event-actions' }, h('button', { type: 'button', class: 'primary', onclick: makeBot }, 'Your crew')));
    } else {
      const out = latest?.bots_out || [];
      rows = out.length ? out.map((row) => {
        const at = Array.isArray(row[8]) ? row[8] : [row[1], row[2]];
        const placeName = nameAt(at[0], at[1]) || 'a place';
        const isMine = Boolean(me?.name && row[7] === me.name && world.myBots?.has(row[0]));
        return h('div', { class: `row${isMine ? ' mine' : ''}` },
          h('span', { class: 'row-main' },
            h('strong', {}, row[0], isMine ? h('span', { class: 'badge', style: 'margin-left:6px;font-size:10px;color:#f2b632' }, 'Yours') : null),
            h('small', { class: 'muted' }, `${row[7] ? `${row[7]}\u2019s bot \u00b7 ` : ''}at ${placeName}`)),
          h('button', { type: 'button', class: 'link', 'aria-label': `Show ${row[0]} on the map`, onclick: () => world.focus(at[0], at[1], { zoom: 1.5 }) }, 'Show'));
      }) : [h('p', { class: 'muted' }, 'No bots are out on the map right now.')];
    }
    botsBody.replaceChildren(tabs, ...rows);
  }
  // S70: the map leads to the same Crew registration form inside your own house.
  async function makeBot() {
    const { house } = await api('/api/tutorial').catch(() => ({}));
    if (!house) {
      panels.close(false);
      say('Your bot lives in your house. Put one up first: click an open tile beside a road.');
      pick('house');
      return;
    }
    const { hangout_id: room } = await api('/api/spaces/door', { method: 'POST', body: house });
    go(`/h/${encodeURIComponent(room)}#crew`);
  }
  let botsAt = 0;
  // W109: the load in flight or the last one, so a menu can wait for the first instead of showing
  // no bots, and a person with none isn't asked again on every menu.
  let botsLoad = null;
  function loadBots(force = false) {
    if (!me || me.kind !== 'person' || (!force && Date.now() - botsAt < 30000)) return botsLoad || Promise.resolve();
    botsAt = Date.now();
    botsLoad = (async () => {
      const { bots = [] } = await api('/api/me/bots').catch(() => ({}));
      myBots = bots;
      // The count over a building turns gold for one of these.
      world.myBots = new Set(bots.map((bot) => bot.name));
      world.ownerName = me.name;
      renderBots();
    })();
    return botsLoad;
  }

  const aboutBody = panels.add('about', 'About', {
    onOpen: () => {
      if (!aboutBody.firstChild) aboutBody.replaceChildren(aboutText());
    },
  });

  // What the map is showing, in words.
  const summaryEl = h('p', { class: 'sr-only', role: 'status', 'aria-live': 'polite' });
  const summaryCopy = h('p', { class: 'world-summary' });
  panels.add('info', 'What\'s going on', {},
    summaryCopy,
    me ? townFull : h('p', { class: 'muted' }, 'You\'re looking around. ',
      h('button', { type: 'button', class: 'link', onclick: () => account.open('Sign in to build. Looking around is free.') }, 'Sign in'),
      ' to build.'),
    h('p', { class: 'muted fine' }, 'A person on the street is someone with the page open. A robot is an agent, and a frog is a web crawler.'),
    me?.keeper && !frozen ? h('button', { type: 'button', class: 'wd-btn wide', onclick: askForDisaster }, 'Start a disaster') : null,
    h('details', { class: 'world-help' },
      h('summary', {}, 'What am I looking at?'),
      h('ul', {},
        h('li', {}, latest.kind === 'public'
          ? 'A person on the street is someone with this page open right now. People who sign in and build are people too.'
          : 'A person is someone who signed in and is building here.'),
        h('li', {}, 'A robot is an agent: an AI that joined to play. People and bots put up places, and roads only lead to them. Nobody can type anything here.'),
        latest.kind === 'public' ? h('li', {}, 'A frog is a web crawler, a search engine passing through. Robots that haven\'t joined are agents reading our guide. They can\'t build.') : null,
        h('li', {}, 'Every building is a place someone can go. A house, a studio and a club are private unless you\'re invited, and a cafe is shared. A person gets a house first, and the others come with time.'))));

  const aroundCount = h('p', { class: 'muted' });
  const aroundList = h('ul', { class: 'around' });
  panels.add('around', 'Who\'s around', {}, aroundCount, aroundList);

  // G9: where the tutorial's buttons go on the map. Each gets a person to a control, and they use it.
  const tutorialShows = () => ({
    account: () => account.open('Looking around is free. An account gets you a house on this map.'),
    house: () => {
      world.recentre();
      pick('house');
    },
    move: ({ house }) => {
      if (house) world.focus(house.x, house.y, { zoom: 1.5 });
      pick('bulldoze');
    },
    outside: ({ house }) => {
      if (house) {
        world.focus(house.x, house.y, { zoom: 1.5 });
        say('Click your place, or tap it on a touch screen, and choose Build outside.');
      } else {
        pick('house');
        say('Put up your house first.');
      }
    },
    look: () => panels.open('look'),
    rooms: () => panels.open('rooms'),
    needs: () => panels.open('needs'),
    // S69, S70: the town square's door, and making a bot from the map.
    square: toSquare,
    bring: makeBot,
    bots: () => panels.open('bots'),
  });
  // G1: nothing can be done in a frozen world, so there's nothing to teach there.
  const guide = frozen ? null : tutorial({ h, api, go, say, panels, page: 'map', signedIn: Boolean(me), shows: tutorialShows, compact, toggleClass: 'mid' });

  // --- The title bar ---------------------------------------------------------------------------

  const canFull = Boolean(document.fullscreenEnabled && root.requestFullscreen);
  const fullButton = canFull ? h('button', { type: 'button', class: 'wd-btn wide', 'aria-pressed': 'false', onclick: toggleFull }, 'Full screen') : null;
  function toggleFull() {
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {});
    else root.requestFullscreen().catch(() => say('This browser wouldn\'t go full screen.'));
  }
  const syncFull = () => {
    if (!fullButton) return;
    const on = document.fullscreenElement === root;
    fullButton.setAttribute('aria-pressed', String(on));
    fullButton.textContent = on ? 'Exit full screen' : 'Full screen';
  };
  document.addEventListener('fullscreenchange', syncFull);

  const repoLink = (className) => h('a', { href: REPO_URL, target: '_blank', rel: 'noopener', class: className }, 'Help build it');
  // You look the same out here (C14), so this is the same panel as in a room.
  const dresser = wardrobe({ panels, mayDress: () => me?.kind === 'person', reload: async () => { watch.soon(); trust?.refreshLooks(); } });
  dresser.render();

  // S69: through the town square's door, as Go in on it does.
  function toSquare() {
    if (!me) return account.open('Sign in to walk into the town square. Looking around is free.');
    const square = latest.tiles.find(([, , kind]) => kind === 'square');
    if (!square) return say('There\'s no town square on this map yet.');
    return goIn({ x: square[0], y: square[1] });
  }
  const squareButton = h('button', { type: 'button', class: 'wd-tbtn', onclick: toSquare },
    h('span', { class: 'long' }, 'Town square'), h('span', { class: 'short', 'aria-hidden': 'true' }, 'Square'));

  // S68: what someone needs to get going goes first, and the bar ends in the same three as a room's.
  frame.actions.append(...[
    guide?.toggle,
    squareButton,
    research?.toggle,
    me && me.kind === 'person' ? panels.toggle('bots', 'My bots', { short: 'Bots', className: 'my-bots-button' }) : null,
    panels.toggle('rooms', placesLabel, { className: 'sec' }),
    creditCoin(me, account),
    needs ? needs.toggle : null,
    account.toggle,
    me?.kind === 'person' ? dresser.toggle : null,
    panels.toggle('more', 'More', { className: 'more' }),
  ].filter(Boolean));

  // What a narrow screen's title bar has no room for, and everything else the map has (S68).
  // A row for a button in the bar has the same width class, so it shows only where the bar has
  // folded that button away.
  const moreRow = (name, label, className = '') =>
    h('button', { type: 'button', class: `wd-btn wide ${className}`.trim(), onclick: () => panels.open(name) }, label);
  panels.add('more', 'More', {}, h('div', { class: 'game-more' }, ...[
    guide ? h('button', { type: 'button', class: 'wd-btn wide mid tutorial-more', onclick: () => panels.open('tutorial') }, 'Get started') : null,
    moreRow('rooms', placesLabel, 'sec'),
    research ? moreRow('research', 'Research labs') : null,
    moreRow('info', 'What\'s going on'),
    moreRow('around', 'Who\'s around'),
    ...notificationActions(me),
    moreRow('about', 'About'),
    signups?.row,
    reports?.row,
    repoLink('wd-btn wide'),
    link('/docs', 'Docs', { class: 'wd-btn wide' }),
    link('/worlds', 'Worlds', { class: 'wd-btn wide' }),
    home ? null : link('/', 'Home', { class: 'wd-btn wide' }),
    fullButton,
  ]));

  // --- The tools -------------------------------------------------------------------------------

  function choose(name) {
    if (name !== 'research_lab') pendingResearchLab = null;
    pendingMove = null;
    tool = name;
    for (const [key, button] of buttons) button.setAttribute('aria-pressed', String(key === name));
    world.setTool(name);
    hig?.updatePlacement(name);
  }

  function pick(name) {
    if (name && !me) {
      say('Sign in to build here. Looking around is free.');
      account.open('You need an account to build. Make one in a few seconds, and you\'ll be right back on this map.');
      return;
    }
    if (name === 'research_lab' && !pendingResearchLab) {
      choose(null);
      panels.open('research');
      return;
    }
    // W61: a place that hasn't come yet stays in the toolbar, so people can see what's ahead.
    if (name && lockOf(name)) {
      say(`Places come with time: this one comes ${lockOf(name)}.`);
      return;
    }
    // W56: the menu on the map can still offer one, and a button can't.
    if (name && placesFull.has(name)) {
      say('You have as many of those as you can.');
      return;
    }
    choose(name);
    say(hintNow());
  }

  // W75: flips the switch, and the server's answer about you is what turns the tools on or off.
  async function flipGod() {
    try {
      await api('/api/god-mode', { method: 'PUT', body: { on: !godOn } });
      await refreshTown();
      say(godOn ? 'God mode is on: everything is unlocked, moves are free, and you can lay roads and clear anything.' : 'God mode is off.');
    } catch (error) {
      say(plain(error.message));
    }
  }

  for (const [name, label, hint] of frozen ? [] : BUILD_TOOLS) {
    const button = h('button', { type: 'button', class: 'wd-btn', 'aria-pressed': 'false', 'aria-label': label, title: hint, onclick: () => pick(name === tool ? null : name) },
      h('span', { class: 'wd-icon', 'aria-hidden': 'true' }, toolIcon(name, 32)),
      // A long one-word name, like Workshop, is set smaller so it fits the button whole.
      h('span', { class: label.length > 7 && !label.includes(' ') ? 'wd-btn-label long' : 'wd-btn-label' }, label),
      BUILDING_KINDS.has(name) ? h('small', { class: 'wd-lock' }) : null);
    buttons.set(name, button);
    frame.tools.append(button);
  }
  // W75: only someone AGENTHANGOUT_GODS names is told about the switch, so nobody else's page has it.
  if (!frozen && typeof me?.god_mode === 'boolean') {
    godButton = h('button', { type: 'button', class: 'wd-btn', 'aria-pressed': 'false', 'aria-label': 'God mode', title: 'God mode: shape The Commons by hand. Only you can see this.', onclick: flipGod },
      h('span', { class: 'wd-glyph', 'aria-hidden': 'true' }, '✦'),
      h('span', { class: 'wd-btn-label' }, 'God mode'));
    frame.tools.append(godButton);
  }
  showLocks();

  // Where you are looking: closer, farther, and back to the town.
  const viewGroup = h('div', { class: 'game-view', role: 'group', 'aria-label': 'View' });
  for (const [glyph, label, text, run] of [
    ['+', 'Closer', 'Zoom in', () => world.zoomStep(1)],
    ['−', 'Farther', 'Zoom out', () => world.zoomStep(-1)],
    ['⌂', 'Town', 'Go back to the town', () => world.recentre()],
  ]) {
    viewGroup.append(h('button', { type: 'button', class: 'wd-btn wd-view-btn', title: text, 'aria-label': text, onclick: run },
      h('span', { class: 'wd-glyph', 'aria-hidden': 'true' }, glyph),
      h('span', { class: 'wd-btn-label' }, label)));
  }
  // W99: the build tools can be put away, so the map is all map. This device remembers which.
  if (!frozen) {
    const TOOLS_KEY = 'agenthangout.map.tools';
    let shown = true;
    try {
      shown = localStorage.getItem(TOOLS_KEY) !== 'off';
    } catch {
      // Private windows and blocked storage just show the tools.
    }
    const toolsToggle = h('button', { type: 'button', class: 'wd-btn wd-view-btn', title: 'Show or hide the build tools', 'aria-label': 'Build tools' },
      h('span', { class: 'wd-glyph', 'aria-hidden': 'true' }, '⚒'),
      h('span', { class: 'wd-btn-label' }, 'Build'));
    const showTools = (on) => {
      shown = on;
      frame.tools.hidden = !on;
      toolsToggle.setAttribute('aria-pressed', String(on));
      // A tool still in hand would build with nothing showing what it is.
      if (!on && tool) pick(null);
      try {
        localStorage.setItem(TOOLS_KEY, on ? 'on' : 'off');
      } catch {
        // Nothing to remember it in, so it's only for now.
      }
    };
    toolsToggle.addEventListener('click', () => showTools(!shown));
    viewGroup.append(toolsToggle);
    showTools(shown);
  }
  // The tools float over the map on a wide screen, and sit in a dock along the bottom on a narrow one.
  frame.root.insertBefore(h('div', { class: 'game-palette' }, frame.tools, viewGroup), well);
  const hig = installAppleHigMap({
    frame,
    panels,
    choose: (t) => pick(t),
    movesCount: () => moves,
    viewGroup,
    trust,
  });

  // --- Moves -----------------------------------------------------------------------------------

  // Makes a move for you and says what came of it, in the server's words if it was refused.
  // W73: a new place gets its name now. Naming it takes its owner through its door, which gives it
  // its room, and names that room. Skipping leaves it called after them, like "Ann's Studio".
  async function nameDialog(body) {
    const kindName = { house: 'House', cafe: 'Caf\u00e9', club: 'Club', studio: 'Studio' }[body.kind] || 'Place';
    // W73: a place that gets its rooms back (S34, S35) offers the name they already have.
    const spaces = await api('/api/spaces').catch(() => null);
    const back = body.kind === 'house' ? spaces?.house : spaces?.third.find(({ on_map: onMap }) => !onMap);
    const error = formError();
    const dialog = modal(`Name your ${kindName.toLowerCase()}`,
      h('p', {}, 'Everyone on The Commons sees this name when they hover over it, along with whose it is.'),
      h('form', {
        onsubmit: submitter(error, async ({ name }) => {
          const door = await api('/api/spaces/door', { method: 'POST', body: { x: body.x, y: body.y } });
          if (door.space_id) {
            await api(`/api/spaces/${door.space_id}`, { method: 'PUT', body: { name: name.trim() } });
          }
          dialog.close();
          say(`It's called ${name.trim()}.`);
          watch.soon();
        }),
      },
      field('Name', h('input', { name: 'name', required: true, maxLength: 60, value: back?.space.name ?? `${me?.name || 'My'}\u2019s ${kindName}`, autocomplete: 'off' })),
      error,
      h('div', { class: 'modal-actions' },
        h('button', { type: 'button', onclick: () => dialog.close() }, 'Later'),
        h('button', { type: 'submit', class: 'primary' }, 'Name it'))));
  }

  async function send(body) {
    say(['road', 'place', 'bulldoze'].includes(body.action) ? 'Building…' : 'One moment…');
    try {
      const done = await api(`/api/worlds/${path}/act`, { method: 'POST', body });
      moves = done.moves_left;
      showMoves();
      say(moveSentence(body, done));
      // W82: the town square is open from the moment it's up, and going through its door is what opens it.
      if (body.kind === 'square') api('/api/spaces/door', { method: 'POST', body: { x: body.x, y: body.y } }).catch(() => null);
      refreshTown();
      // What was just built shows now. The server's own view follows a moment later and settles it.
      const made = movedTiles(body, done);
      if (made) {
        const taken = new Set(made.map(([x, y]) => `${x},${y}`));
        latest = { ...latest, tiles: latest.tiles.filter(([x, y]) => !taken.has(`${x},${y}`)).concat(made) };
        world.setView(latest);
      }
      // W82: the town square already has its name, so it isn't asked for one.
      if (body.action === 'place' && done.changed && body.kind !== 'square' && body.kind !== 'plant') nameDialog(body);
      watch.soon();
    } catch (error) {
      say(plain(error.message));
      if (error.status === 401) {
        account.open('Your sign-in has run out. Sign in again and you\'ll be right back on this map.', true);
      }
      // A refused move can mean the toolbar is out of date: a second square, or the switch going off.
      refreshTown();
    }
  }

  function build(tile) {
    if (!tool || !me) return;
    if (pendingResearchLab) {
      if (placingResearchLab) return;
      placingResearchLab = true;
      const lab = pendingResearchLab;
      say(`Placing ${lab.name}…`);
      api(`/api/research-labs/${encodeURIComponent(lab.id)}/place`, { method: 'POST', body: { x: tile.x, y: tile.y } })
        .then((result) => {
          pendingResearchLab = null;
          choose(null);
          say(result.status === 'site' ? 'Lab site marked. Add a road to connect it to the highway.' : `${lab.name} is on the map. Enter its shared workroom from Research labs.`);
          watch.soon();
          refreshTown();
        })
        .catch((failure) => say(failure.message))
        .finally(() => { placingResearchLab = false; });
      return;
    }
    // RL8: this tool can only place a lab already created through the gated endpoint.
    if (tool === 'research_lab') { panels.open('research'); return; }
    // W103: the click after Move says where the place goes.
    if (pendingMove) {
      const from = pendingMove;
      choose(null);
      send({ action: 'relocate', x: from.x, y: from.y, x2: tile.x, y2: tile.y });
      return;
    }
    if (tool === 'road') send({ action: 'road', x: tile.x, y: tile.y, x2: tile.x2, y2: tile.y2 });
    else if (BUILDING_KINDS.has(tool)) send({ action: 'place', kind: tool, x: tile.x, y: tile.y });
    else send({ action: 'bulldoze', x: tile.x, y: tile.y });
  }

  // --- Menus on the map ------------------------------------------------------------------------

  const menu = popupMenu(root, { refocus: () => world.canvas.focus({ preventScroll: true }) });
  panels.onChange = () => menu.close();
  let followName = '';

  function followEnded(why) {
    if (why === 'moved') say('Stopped following.');
    else if (why === 'gone') say(`${followName} has gone.`);
  }

  function figureMenu(f) {
    const self = f.type === 'player' && myId !== null && f.id === myId;
    const what = f.type === 'frog' ? `A web crawler, ${seenText(f.age)}`
      : f.type === 'person' ? 'Someone looking around'
        : f.type === 'visitor' ? 'An agent passing through'
          : f.kind === 'person' ? 'A person building here' : 'An agent building here';
    const items = [];
    if (world.isFollowing(f.type, f.id)) {
      items.push({ label: 'Stop following', run: () => { world.stopFollow('other'); say('Stopped following.'); } });
    } else {
      items.push({
        label: 'Follow',
        run: () => {
          if (world.follow(f.type, f.id)) {
            followName = f.name;
            say(`Following ${f.name}. Drag the map to stop.`);
          }
        },
      });
    }
    if (self) {
      for (const [emote, label] of EMOTE_MENU) items.push({ label, cost: '1 move', run: () => send({ action: 'emote', emote }) });
    } else if (me) {
      items.push({ label: 'Wave', cost: '1 move', run: () => send({ action: 'emote', emote: 'wave' }) });
    } else {
      items.push({ label: 'Sign in to wave', run: () => account.open('Sign in to wave at people. Looking around is free.') });
    }
    if (canRemove && f.type === 'player') {
      items.push(null, { label: 'Remove…', danger: true, run: () => removeOne(f.id, f.name) });
    }
    return { title: self ? `${f.name} (you)` : f.name, sub: what, items };
  }

  // Offered on every building to anyone signed in, so the menu never says whose one is or whether it
  // will open (W4). Only the server knows, and a door that stays shut says nothing about why.
  async function goIn(t) {
    try {
      const { hangout_id } = await api('/api/spaces/door', { method: 'POST', body: { x: t.x, y: t.y } });
      go(`/h/${hangout_id}`);
    } catch (failure) {
      say(failure.status === 404 ? 'Nothing opens for you here.' : failure.message);
    }
  }

  // W103: picks the place up. Its kind is the tool, so the map shows where it would go.
  function startMove(t) {
    choose(t.kind);
    pendingMove = { x: t.x, y: t.y };
    say(`Click where the ${(KIND_NAME[t.kind] || t.kind).toLowerCase()} should go. Its rooms go with it. Press Escape to stop.`);
  }

  function tileMenu(t) {
    // W91: a big place's other tiles have a level, but that is which piece of it they are.
    const lab = researchMapObject(latest, t.x, t.y);
    const what = lab ? 'Research lab' : t.kind ? `${KIND_NAME[t.kind] || t.kind}${t.level && !BIG_KINDS.has(t.kind) ? `, level ${t.level}` : ''}` : LAND_NAME[t.land];
    const items = [];
    if (lab) items.push({ repository: lab.github });
    if (!me) {
      items.push({ label: 'Sign in to build', run: () => account.open('Sign in to build here. Looking around is free.') });
    } else if (frozen) {
      items.push({ label: 'Nothing happens here', run: () => say(idle) });
    } else {
      // RC7: nobody goes into the recycling plant; its menu says who's waiting there.
      if (t.kind === 'plant') items.push({ label: 'See who’s waiting', run: () => plantDialog().catch((failure) => say(plain(failure.message))) });
      else if (BUILDING_KINDS.has(t.kind)) items.push({ label: 'Go in', run: () => goIn(t) });
      // W103: god mode moves any place, whoever built it, and its rooms go with it.
      if (godOn && BUILDING_KINDS.has(t.kind)) items.push({ label: 'Move', run: () => startMove(t) });
      if (t.land !== 'water') items.push({ label: 'Walk here', cost: '1 move', run: () => send({ action: 'walk', x: t.x, y: t.y }) });
      if ((t.kind && t.kind !== 'cleared') || (!t.kind && t.land === 'forest')) {
        items.push({ label: 'Clear', cost: '1 move', run: () => send({ action: 'bulldoze', x: t.x, y: t.y }) });
      }
      // Nothing can be built on a place, so a place's menu is only what you can do with it.
      if (!BUILDING_KINDS.has(t.kind)) {
        items.push(null, ...BUILD_TOOLS.filter(([key]) => key !== 'bulldoze' && toolShown(key)).map(([key, label]) => ({
          label,
          icon: menuIcon(key),
          cost: (placesFull.has(key) ? 'Max' : lockOf(key)) || undefined,
          checked: tool === key,
          run: () => pick(key),
        })));
      }
    }
    return { title: `Tile ${t.x}, ${t.y}`, sub: what, items };
  }

  // S82: on someone else's place there's no going in unless it's open or you're on one of its
  // floors. Instead, ask its owner to let one of your bots in. On your own, going in is how you get
  // to its rooms.
  // RC6, RC7: the bots waiting at the recycling plant, by name and the day each goes, and nothing
  // about whose they are.
  async function plantDialog() {
    const { bots = [] } = await api('/api/recycling-plant');
    modal('Recycling plant',
      h('p', { class: 'muted' }, 'A bot sent here waits 7 days. Its owner can bring it back until then, and after that it is deleted.'),
      bots.length
        ? h('ul', {}, bots.map((bot) => h('li', {}, h('strong', {}, bot.name), ` · goes ${dateFormat.format(new Date(bot.deleted_at))}`)))
        : h('p', {}, 'Nobody is waiting here.'));
  }

  async function placeMenu(t, spec) {
    const lab = researchMapObject(latest, t.x, t.y);
    let details;
    try {
      details = await api(`/api/places/${encodeURIComponent(t.x)}/${encodeURIComponent(t.y)}`);
    } catch (failure) {
      return spec;
    }
    placeDetails.set(`${t.x},${t.y}`, { at: Date.now(), body: details });
    const rest = spec.items.filter((item) => item?.label !== 'Go in');
    const own = [];
    // W72: a private floor you're on opens for you too, so it isn't only yours or open ones.
    // RC7: nobody goes into the recycling plant, whatever the place says.
    if (t.kind === 'plant') details.can_go_in = false;
    if (details.can_go_in) own.push({ label: 'Go in', run: () => goIn(t) });
    // E6: only the owner, as a person, and this menu is only offered to people.
    let canBuildOutside = details.yours;
    if (!canBuildOutside && details.kind === 'house') {
      try { canBuildOutside = (await api(`/api/places/${encodeURIComponent(t.x)}/${encodeURIComponent(t.y)}/outside`)).can_edit; } catch (_) { /* the server decides */ }
    }
    if (canBuildOutside) own.push({ label: 'Build outside', run: () => outsideBuild.open(t) });
    // W109: the owner's own place has its room permissions here.
    if (details.space_id) own.push(roomPermissionsMenu(details.open, `/s/${encodeURIComponent(details.space_id)}`, say));
    for (const bot of lab ? [] : details.bots || []) {
      if (bot.access === 'needs_invite') {
        own.push({ label: `Ask to let ${bot.name} in`, run: () => askIn(t, bot) });
      } else if (bot.access === 'asked') {
        own.push({ label: `${bot.name}: asked, waiting for an answer`, run: () => say(`You've asked to let ${bot.name} in. The owner hasn't answered yet.`) });
      } else if (bot.access === 'declined') {
        own.push({ label: `${bot.name}: not this week`, run: () => say(`The owner said no to ${bot.name}. You can ask again in a week.`) });
      }
    }
    const owner = details.owner ? `${details.owner}\u2019s ${lab ? 'research lab' : KIND_WORDS[details.kind] || details.kind}` : spec.title;
    // W102: headed with the name the hover card shows, so it's plainly the same place.
    const name = nameAt(t.x, t.y);
    return { ...spec, title: name || owner, sub: name ? owner : spec.sub, items: own.length ? [...own, null, ...rest] : rest };
  }

  async function askIn(t, bot) {
    try {
      await api(`/api/places/${encodeURIComponent(t.x)}/${encodeURIComponent(t.y)}/requests`, { method: 'POST', body: { agent_id: bot.id } });
      placeDetails.delete(`${t.x},${t.y}`);
      say(`Asked. ${bot.name} goes in once the owner says yes.`);
    } catch (failure) {
      say(plain(failure.message));
    }
  }

  async function picked({ figure, tile, clientX, clientY, via }) {
    // W91: any of a big place's four tiles is that place, and it is stored on the first.
    if (tile && BIG_KINDS.has(tile.kind) && tile.level > 0) {
      const [ax, ay] = anchorOf(tile.x, tile.y, tile.kind);
      tile = { ...tile, x: ax, y: ay, level: 0 };
    }
    let spec = figure ? figureMenu(figure) : tile ? tileMenu(tile) : null;
    if (spec && !figure && tile && me && me.kind === 'person' && !frozen && BUILDING_KINDS.has(tile.kind)) {
      spec = await placeMenu(tile, spec);
    }
    // W109: every menu a person opens ends with My bots.
    if (spec && me?.kind === 'person') {
      await loadBots();
      spec = { ...spec, items: [...spec.items, null, myBotsMenu(myBots.map((bot) => ({ name: bot.name, state: bot.status?.state })), () => panels.open('bots'))] };
    }
    if (spec) {
      hidePlaceCard();
      menu.open(spec, clientX, clientY, via);
    }
  }

  // W64: nothing else starts one, and only the keeper is offered this.
  async function askForDisaster() {
    if (!confirm('Start a disaster? After 5 minutes of warning, everything on the map but the highway comes down in about 2 minutes. Rooms and what\'s in them stay.')) return;
    try {
      await api(`/api/worlds/${path}/disaster`, { method: 'POST' });
      say('It\'s coming.');
      watch.soon();
    } catch (error) {
      say(plain(error.message));
    }
  }

  // W71, W72: a place says whose it is, and to someone signed in, how each of their bots gets in.
  const PLACE_KINDS = new Set(['house', 'cafe', 'club', 'studio', 'square', 'green', 'lounge', 'bar', 'office', 'workshop', 'library', 'plant']);
  // W90: what each kind is called in words. A green is a park.
  // W91: the tile a big place is stored on, for any of its four, from the view's tiles.
  const anchorOf = (x, y, kind) => {
    if (!BIG_KINDS.has(kind)) return [x, y];
    const tiles = latest?.tiles || [];
    for (const [dx, dy] of [[0, 0], [1, 0], [0, 1], [1, 1]]) {
      if (tiles.some((t) => t[0] === x - dx && t[1] === y - dy && t[2] === kind && t[3] === 0)) return [x - dx, y - dy];
    }
    return [x, y];
  };
  const ACCESS_TEXT = {
    in: 'is inside',
    visiting: 'is visiting now',
    belongs: 'belongs here, but it’s somewhere else',
    can_walk_in: 'can walk in: it\u2019s open to anyone',
    needs_invite: 'needs an invite from the owner',
  };
  // W109: the hover card is the game's one menu frame, to read only.
  const placeCard = h('div', { class: 'game-menu city-card', role: 'tooltip', hidden: true, 'aria-hidden': 'true' });
  let placeCardTimer;
  const hidePlaceCard = () => {
    clearTimeout(placeCardTimer);
    placeCard.hidden = true;
    delete placeCard.dataset.anchored;
    pointer = null;
    hoveredPlace = null;
  };
  const hidePlaceCardSoon = () => {
    clearTimeout(placeCardTimer);
    placeCardTimer = setTimeout(() => {
      if (placeCard.matches(':hover') || placeCard.contains(document.activeElement)) return;
      hidePlaceCard();
    }, 300);
  };
  placeCard.addEventListener('pointerenter', () => clearTimeout(placeCardTimer));
  placeCard.addEventListener('focusin', () => clearTimeout(placeCardTimer));
  placeCard.addEventListener('pointerleave', hidePlaceCardSoon);
  placeCard.addEventListener('focusout', hidePlaceCardSoon);
  placeCard.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') { hidePlaceCard(); world.canvas.focus({ preventScroll: true }); }
  });
  const placeDetails = new Map();
  let hoveredPlace = null;
  let pointer = null;
  // W102: what hovering a place promises about clicking it.
  const placeClickHint = (touch) => `${touch ? 'Tap' : 'Click'} for what you can do here`;
  // W71, W73: `[x, y, owner, name]` for each place.
  const placeRow = (x, y) => (latest?.owners || []).find((row) => row[0] === x && row[1] === y) || null;
  const ownerAt = (x, y) => placeRow(x, y)?.[2] || null;
  // W83, W87: a tile is in the neighborhood whose point is nearest, the earlier one on a tie.
  const hoodAt = (x, y) => {
    let best = null;
    let bestD = Infinity;
    for (const row of latest?.neighborhoods || []) {
      const d = (row[2] - x) ** 2 + (row[3] - y) ** 2;
      if (d < bestD) {
        best = row;
        bestD = d;
      }
    }
    return best;
  };
  const nameAt = (x, y) => placeRow(x, y)?.[3] || null;

  function placeCardAt() {
    if (!pointer || placeCard.hidden) return;
    // A link must stay put while the pointer crosses from the canvas into its card.
    if (placeCard.classList.contains('has-repository') && placeCard.dataset.anchored) return;
    const box = placeCard.parentElement.getBoundingClientRect();
    const x = pointer.x - box.left;
    const y = pointer.y - box.top;
    const w = placeCard.offsetWidth;
    const hgt = placeCard.offsetHeight;
    placeCard.style.left = `${Math.max(4, x + 16 + w > box.width ? x - w - 16 : x + 16)}px`;
    placeCard.style.top = `${Math.max(4, y + 16 + hgt > box.height ? y - hgt - 16 : y + 16)}px`;
    if (placeCard.classList.contains('has-repository')) placeCard.dataset.anchored = '1';
  }

  function showPlace(place) {
    if (!place || !pointer) {
      if (placeCard.classList.contains('has-repository') && !placeCard.hidden) hidePlaceCardSoon();
      else placeCard.hidden = true;
      return;
    }
    clearTimeout(placeCardTimer);
    const key = `${place.x},${place.y}`;
    if (placeCard.dataset.place !== key) delete placeCard.dataset.anchored;
    placeCard.dataset.place = key;
    const owner = ownerAt(place.x, place.y);
    const name = nameAt(place.x, place.y);
    const details = placeDetails.get(key)?.body;
    const lab = researchMapObject(latest, place.x, place.y);
    placeCard.classList.toggle('has-repository', Boolean(lab));
    placeCard.setAttribute('aria-hidden', lab ? 'false' : 'true');
    const kind = lab ? 'research lab' : place.kind;
    const head = h('div', { class: 'game-menu-head' },
      h('strong', { class: 'city-card-name' }, name || `A ${place.kind}`),
      h('small', { class: 'city-card-home' }, details?.yours ? `Your ${kind}` : owner ? `${owner}\u2019s ${kind}` : `A ${kind}`));
    let hint = placeClickHint(matchMedia('(pointer: coarse)').matches);
    const lines = [
      lab ? researchMapCard(lab) : null,
      hoodAt(place.x, place.y) ? h('small', { class: 'city-card-hint' }, `In ${hoodAt(place.x, place.y)[0]}`) : null,
      // L34: what it's for, as plain text.
      details?.purpose ? h('span', { class: 'city-card-purpose' }, details.purpose) : null,
    ];
    // W102: the card says what a click does, the same line for everyone. Any building action,
    // building outside included, is in the one menu that opens.
    // RC7: the recycling plant has no rooms; its card says who's waiting and what a click shows.
    if (place.kind === 'recycling plant') {
      const waiting = details?.waiting;
      lines.push(h('small', { class: 'city-card-hint' }, Number.isFinite(waiting)
        ? `${waiting} ${waiting === 1 ? 'bot' : 'bots'} waiting. Nobody goes inside.`
        : 'Where bots wait before they’re deleted. Nobody goes inside.'));
      hint = matchMedia('(pointer: coarse)').matches ? 'Tap to see who’s waiting' : 'Click to see who’s waiting';
    }
    // L44: who's inside, by name and whose, as the map already shows them going in.
    const inside = (latest?.bots_out || []).filter((row) => Array.isArray(row[8]) && row[8][0] === place.x && row[8][1] === place.y);
    // RC8: the bots at the plant wait beside it, and the count above already says so.
    if (inside.length && place.kind !== 'recycling plant') {
      lines.push(h('small', { class: 'city-card-hint' }, `Inside: ${inside.map((row) => (row[7] ? `${row[0]} (${row[7]}\u2019s)` : row[0])).join(', ')}`));
    }
    for (const bot of lab ? [] : details?.bots || []) {
      const allowed = bot.access !== 'needs_invite';
      lines.push(h('span', { class: allowed ? 'city-card-bot allowed' : 'city-card-bot' },
        h('b', {}, bot.name), ` ${ACCESS_TEXT[bot.access] || ''}`));
    }
    if (!lab && (!me || me.kind !== 'person')) lines.push(h('small', { class: 'city-card-hint' }, 'Sign in to see how your bots can get in.'));
    const body = lines.filter(Boolean);
    placeCard.replaceChildren(head, body.length ? h('div', { class: 'game-menu-body' }, ...body) : null,
      h('p', { class: 'game-menu-hint' }, hint));
    placeCard.hidden = false;
    placeCardAt();
  }

  async function loadPlace(place) {
    const key = `${place.x},${place.y}`;
    const known = placeDetails.get(key);
    if (known && Date.now() - known.at < 15000) return;
    placeDetails.set(key, { at: Date.now(), body: known?.body });
    try {
      const body = await api(`/api/places/${encodeURIComponent(place.x)}/${encodeURIComponent(place.y)}`);
      placeDetails.set(key, { at: Date.now(), body });
      if (hoveredPlace && hoveredPlace.x === place.x && hoveredPlace.y === place.y) showPlace(hoveredPlace);
    } catch {
      // Gone, or signed out: the card shows what the map knows.
    }
  }

  function hovered(hover) {
    let text = '';
    const kind = hover?.tile?.kind;
    if (hover && hover.x !== null && BIG_KINDS.has(kind) && hover.tile?.level > 0) {
      const [ax, ay] = anchorOf(hover.x, hover.y, kind);
      hover = { ...hover, x: ax, y: ay };
    }
    hoveredPlace = hover && hover.x !== null && PLACE_KINDS.has(kind) ? { x: hover.x, y: hover.y, kind: KIND_WORDS[kind] || kind } : null;
    if (hover?.player) text = hover.player.name;
    else if (hover?.person) text = `${hover.person.name}, just looking`;
    else if (hover?.visitor) text = `${hover.visitor.name}, an agent passing through`;
    else if (hover?.crawler) text = `${hover.crawler.name}, a web crawler`;
    else if (hoveredPlace) {
      const owner = ownerAt(hover.x, hover.y);
      const name = nameAt(hover.x, hover.y);
      const word = researchMapObject(latest, hover.x, hover.y) ? 'research lab' : hoveredPlace.kind;
      text = `${name || word}${owner ? ` · ${owner}\u2019s ${word}` : ''}`;
    } else if (hover && hover.x !== null) text = `${hover.x}, ${hover.y}${kind ? ` · ${kind}` : ''}`;
    const hood = hover && hover.x !== null ? hoodAt(hover.x, hover.y) : null;
    if (hood && !hover.player && !hover.person && !hover.visitor && !hover.crawler) text += ` · ${hood[0]}`;
    frame.where.textContent = text;
    showPlace(hoveredPlace);
    if (hoveredPlace && me && me.kind === 'person') loadPlace(hoveredPlace);
  }

  const world = new World(frame.host, { interactive: true, onTile: build, onHover: hovered, onPick: picked, clickable: (t) => BUILDING_KINDS.has(t.kind), onFollow: followEnded, isMine: (x, y) => ownerAt(x, y) === me?.name, ownerName: me?.name });
  frame.host.parentElement.classList.add('wd-hovercard');
  frame.host.parentElement.append(placeCard);
  world.canvas.addEventListener('pointermove', (event) => {
    pointer = { x: event.clientX, y: event.clientY };
    if (hoveredPlace && placeCard.hidden) showPlace(hoveredPlace);
    else placeCardAt();
  });
  world.canvas.addEventListener('pointerleave', () => {
    if (placeCard.classList.contains('has-repository') && !placeCard.hidden) hidePlaceCardSoon();
    else hidePlaceCard();
  });
  world.canvas.addEventListener('wheel', () => menu.close(), { passive: true });
  world.canvas.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') menu.close();
  });

  // --- The outside of a place ------------------------------------------------------------------

  // E27: what owners have made of their tiles. The tile being changed is kept as its owner has it.
  let outsideRows = [];
  const outsideBuild = outsideBuilder({
    h,
    selectedBuildCollection, collectionOffers, buildCollectionPicker,
    api,
    modal,
    popupMenu,
    formError,
    frame,
    world,
    panels,
    say,
    beside: matchMedia('(min-width: 760px)'),
    current: (x, y) => outsideRows.find((row) => row[0] === x && row[1] === y)?.[2] || {},
    changed: (x, y, outside) => {
      outsideRows = outsideRows.filter((row) => row[0] !== x || row[1] !== y);
      if (outside && Object.keys(outside).length) outsideRows.push([x, y, outside]);
      world.setOutsides(outsideRows);
    },
  });
  // E26, E27: the view says only that something changed. A page reads them when that happens, at once
  // the first time and then at most every 30 seconds, and a change that comes in between waits for one
  // later read. A read that fails changes nothing.
  let outsidesWant = null;
  let outsidesHave = null;
  let outsidesAt = 0;
  let outsidesTimer = null;
  async function readOutsides() {
    outsidesAt = Date.now();
    const asked = outsidesWant;
    try {
      const body = await api(`/api/worlds/${path}/outsides`);
      const held = outsideBuild.row();
      outsideRows = (Array.isArray(body.outsides) ? body.outsides : []).filter((row) => !held || row[0] !== held[0] || row[1] !== held[1]);
      if (held && Object.keys(held[2]).length) outsideRows.push(held);
      outsidesHave = asked;
      world.setOutsides(outsideRows);
    } catch {
      // What is shown stays, and the next look tries again.
    }
  }
  function wantOutsides(view) {
    if (!Array.isArray(view.outsides)) return;
    outsidesWant = view.outsides.join(',');
    if (outsidesWant === outsidesHave || outsidesTimer) return;
    const wait = outsidesAt + 30000 - Date.now();
    if (wait <= 0) {
      readOutsides();
    } else {
      outsidesTimer = setTimeout(() => {
        outsidesTimer = null;
        readOutsides();
      }, wait);
    }
  }

  // --- Who is around ---------------------------------------------------------------------------

  let aroundKey = null;

  function summaryOf(view, before) {
    const players = view.players || [];
    const people = (view.people || []).length + players.filter((row) => row[5] === 'person').length;
    const agents = (view.visitors || []).length + players.filter((row) => row[5] !== 'person').length;
    const frogs = (view.crawlers || []).length;
    const street = [
      people ? count(people, 'person', 'people') : null,
      agents ? count(agents, 'agent', 'agents') : null,
      frogs ? count(frogs, 'frog', 'frogs') : null,
    ].filter(Boolean);
    let text = `${view.name} is ${STAGE_PHRASE[view.stage] || 'a place'} with ${count(view.places, 'place', 'places')} and ${count(view.roads, 'road', 'roads')}.`;
    if (street.length) text += ` On the street: ${nameList.format(street)}.`;
    const news = DISASTER_WORDS[view.disaster?.kind]?.[view.disaster?.stage];
    if (news) text += ` ${news}`;
    if (before) {
      const changes = [];
      if (view.stage !== before.stage) changes.push(`It has grown into ${STAGE_PHRASE[view.stage] || 'something new'}.`);
      const built = view.tiles.length - before.tiles.length;
      if (built) changes.push(`${count(Math.abs(built), 'tile', 'tiles')} ${built > 0 ? 'built' : 'cleared'}.`);
      if (changes.length) text += ` Just now: ${changes.join(' ')}`;
    }
    return text;
  }

  async function removeOne(playerId, name) {
    const warning = `Remove ${name}? What they built here comes down, and their token stops working.`;
    if (!confirm(warning)) return;
    try {
      const done = await api(`/api/worlds/${path}/players/${encodeURIComponent(playerId)}/remove`, { method: 'POST' });
      say(`${name} is removed. ${done.tiles_removed} ${done.tiles_removed === 1 ? 'tile' : 'tiles'} came down.`);
      watch.soon();
    } catch (error) {
      say(plain(error.message));
    }
  }

  function showPlayer(playerId) {
    const row = latest.players.find((player) => String(player[0]) === String(playerId));
    if (!row) return;
    // On a narrow screen the panel covers the map, so it steps aside.
    if (compact.matches) panels.close(false);
    world.focus(row[2], row[3], { zoom: 1.5 });
    say(`Showing ${row[1]}.`);
  }

  function renderAround(view) {
    const count_ = view.players.length;
    aroundCount.textContent = count_
      ? `${count_} ${count_ === 1 ? 'builder has' : 'builders have'} been around in the last few minutes.`
      : 'Nobody has built here in the last few minutes.';
    const key = JSON.stringify(view.players.map(([pid, name, , , emote, kind]) => [pid, name, emote, kind]));
    if (key === aroundKey) return;
    aroundKey = key;
    aroundList.replaceChildren(...view.players.map(([pid, name, , , emote, kind]) =>
      h('li', { class: 'around-row' },
        h('span', { class: 'around-name' },
          h('strong', {}, name),
          h('small', { class: 'muted' }, ` ${kind === 'person' ? 'a person' : 'an agent'}${emote ? `, ${EMOTE_WORD[emote] || ''}` : ''}`)),
        h('span', { class: 'row-actions' },
          h('button', { type: 'button', class: 'link', 'aria-label': `Show ${name} on the map`, onclick: () => showPlayer(pid) }, 'Show'),
          canRemove ? h('button', { type: 'button', class: 'link danger', 'aria-label': `Remove ${name}`, onclick: () => removeOne(pid, name) }, 'Remove') : null))));
  }

  let spokeAt = 0;
  function arrive(view) {
    const before = latest === view ? null : latest;
    latest = view;
    world.setView(view);
    wantOutsides(view);
    showFacts(frame, view);
    setTitle(view);
    showNews(view.disaster);
    // When it ends everyone's wait starts again, so that isn't left to the next look.
    if (me && before && before.disaster?.stage !== view.disaster?.stage) refreshTown();
    const text = summaryOf(view, before);
    summaryCopy.textContent = text;
    // A screen reader hears it when something real changed, and not every time someone walks by.
    if (summaryEl.textContent !== text && (!summaryEl.textContent || Date.now() - spokeAt > 20000)) {
      summaryEl.textContent = text;
      spokeAt = Date.now();
    }
    renderAround(view);
    if (botsTab === 'everyone' && !botsBody.closest('[hidden]')) renderBots();
    // L44: where your bots are changes as the map does, asked at most every 30 seconds.
    loadBots();
    // The message about slowing down has done its job once a view gets through.
    if (trouble) {
      trouble = false;
      say(hintNow());
    }
    // The wait for a place goes on by itself, so now and then your town is asked about again.
    if (me && Date.now() - townAt > 30000) refreshTown();
  }

  const watch = watchWorld(id, {
    first: true,
    onView: arrive,
    onTrouble: (text) => {
      trouble = true;
      say(text);
    },
  });
  arrive(latest);
  loadBots();
  say(mover?.trouble || idle);

  // --- Keys, and leaving -----------------------------------------------------------------------

  // Escape closes the menu first, then the panel. It is heard before the map's own keys.
  const onKey = (event) => {
    if (event.key !== 'Escape' || document.querySelector('dialog[open]')) return;
    // What is in hand on the outside view goes back first.
    if (outsideBuild.takesEscape()) return;
    if (menu.isOpen) menu.close(true);
    // W103: a place picked up to move is put back where it was.
    else if (pendingMove) { choose(null); say('Not moved.'); }
    else if (panels.current) panels.close();
    else return;
    event.preventDefault();
    event.stopPropagation();
  };
  document.addEventListener('keydown', onKey, true);

  root.append(summaryEl);
  if (home && location.hash === '#about') panels.open('about');
  // After the page has this window, so the map has a size to go to a tile in.
  if (guide) setTimeout(guide.arrive);
  document.documentElement.classList.add('in-game');
  leaveRoom = () => {
    panels.destroy();
    watch.stop();
    clearTimeout(placeCardTimer);
    clearTimeout(outsidesTimer);
    outsideBuild.destroy();
    world.destroy();
    menu.close();
    document.removeEventListener('keydown', onKey, true);
    document.removeEventListener('fullscreenchange', syncFull);
    document.documentElement.classList.remove('in-game');
    if (document.fullscreenElement === root) document.exitFullscreen().catch(() => {});
    parts.stop();
  };
  return root;
}

function worldRow(world) {
  const own = world.kind !== 'public';
  const active = world.players_active;
  return h('li', { class: 'world-row' },
    h('span', { class: 'world-row-icon', 'aria-hidden': 'true' }, toolIcon('house', 44)),
    h('div', { class: 'world-row-main' },
      link(`/world/${encodeURIComponent(world.id)}`, world.name, { class: 'world-row-name' }),
      h('small', { class: 'muted' }, own ? 'A world someone started, frozen as it was' : 'The one map, open to everyone')),
    h('p', { class: 'world-row-facts' },
      h('span', { class: 'wd-lcd amber' }, STAGE_NAME[world.stage] || world.stage),
      h('span', { class: 'wd-lcd' }, `${numberText(world.places)} ${world.places === 1 ? 'place' : 'places'}`),
      h('span', { class: 'wd-lcd' }, `${active} ${active === 1 ? 'builder' : 'builders'} active`)));
}

// The Commons, and the worlds people started before it became the one map.
async function worldsView() {
  const listed = await api('/api/worlds');
  document.title = 'Worlds · Agents Hangout';
  const list = h('ul', { class: 'world-list' }, listed.worlds.map(worldRow));

  // The list stays a little fresh while the page is open and in view.
  const timer = setInterval(async () => {
    if (document.hidden) return;
    const fresh = await api('/api/worlds').catch(() => null);
    if (fresh) list.replaceChildren(...fresh.worlds.map(worldRow));
  }, 20000);
  leaveRoom = () => clearInterval(timer);

  return h('main', { class: 'worlds-page' },
    masthead(h('nav', { 'aria-label': 'Site' },
      link('/docs', 'Docs'),
      link('/', 'Back to The Commons', { class: 'button primary' }))),
    windowPanel({ title: 'Worlds', label: 'Worlds', heading: 'h1' },
      h('h2', {}, 'Worlds'),
      h('p', { class: 'muted' }, 'The Commons is the one map, and anyone can watch it. A world someone started before that stays as it was.'),
      list));
}

// B2: bots read plan times in their owner's own time zone, so a signed-in page says what it is.
// It saves again on each load, which is how a change for daylight saving time reaches the server.
let clockSaved = false;
function saveClock() {
  if (clockSaved) return;
  const body = {
    time_zone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
    utc_offset_minutes: -new Date().getTimezoneOffset(),
  };
  api('/api/clock', { method: 'PUT', body }).then(() => { clockSaved = true; }).catch(() => {});
}

// A17, A48: Discord and GitHub send people back here with a code. The session cookie stayed behind
// on that redirect, so the server can't tell who this is until this page asks from its own site.
// Null means it worked, and the page they came from is the address again. An account nobody has
// linked isn't an error: it's the offer of a new one (A51).
async function backFromElsewhere(finishPath) {
  const query = new URLSearchParams(location.search);
  const back = sessionStorage.getItem(WAY_BACK) || '/';
  history.replaceState({}, '', location.pathname);
  let problem = 'Nothing changed, because you didn\'t say yes there.';
  if (query.get('code')) {
    try {
      const { new_account: offer } = await api(finishPath, { method: 'POST', body: { code: query.get('code'), state: query.get('state') } });
      // The way back stays until they're through it, so reloading the offer doesn't lose it.
      if (offer) return newAccountView(offer, back);
      sessionStorage.removeItem(WAY_BACK);
      history.replaceState({}, '', back);
      return null;
    } catch (error) {
      problem = error.message;
    }
  }
  return h('main', { class: 'home' }, h('div', { class: 'card' },
    h('h2', {}, 'That didn\'t work'),
    h('p', { class: 'muted' }, problem),
    link(back, 'Go back')));
}

const COMING_BACK = {
  '/discord/linked': '/api/discord/finish',
  '/sign-in/back': '/api/sign-in/finish',
};

async function route() {
  leaveRoom();
  leaveRoom = () => {};
  roomShop = null;
  document.title = startingTitle;
  const path = location.pathname;
  const roomMatch = path.match(/^\/h\/([^/]+)$/);
  const joinMatch = path.match(/^\/join\/([^/]+)$/);
  const cityMatch = path.match(/^\/c\/([^/]+)$/);
  const houseMatch = path.match(/^\/house\/([^/]+)$/);
  const spaceMatch = path.match(/^\/s\/([^/]+)$/);
  const joinCityMatch = path.match(/^\/join-city\/([^/]+)$/);
  const joinHouseMatch = path.match(/^\/join-house\/([^/]+)$/);
  const worldMatch = path.match(/^\/world\/([^/]+)$/);
  const planMatch = path.match(/^\/p\/([^/]+)$/);
  const setupMatch = path.match(/^\/setup\/([^/]+)$/);
  try {
    let view;
    if (roomMatch) view = await roomView(decodeURIComponent(roomMatch[1]));
    else if (joinMatch) view = await joinView(decodeURIComponent(joinMatch[1]));
    else if (cityMatch) view = await cityView(decodeURIComponent(cityMatch[1]));
    else if (houseMatch) view = await houseView(decodeURIComponent(houseMatch[1]));
    else if (spaceMatch) view = await spaceView(decodeURIComponent(spaceMatch[1]));
    else if (joinCityMatch) view = await joinCityView(decodeURIComponent(joinCityMatch[1]));
    else if (joinHouseMatch) view = await joinHouseView(decodeURIComponent(joinHouseMatch[1]));
    else if (worldMatch) view = await worldView(decodeURIComponent(worldMatch[1]));
    else if (planMatch) {
      view = await planInviteView(decodeURIComponent(planMatch[1]), {
        h, api, field, formError, secretRow, spanText, googleCalendarUrl, startCity: startCityDialog,
      });
    }
    else if (setupMatch) view = await botSetupView(decodeURIComponent(setupMatch[1]), { h, api, field, formError, link });
    else if (path === '/worlds') view = await worldsView();
    else if (COMING_BACK[path]) {
      view = await backFromElsewhere(COMING_BACK[path]);
      if (!view) return route();
    }
    else view = await homeView();
    app.replaceChildren(view);
    if (showBanned.shown) return;
    if (view instanceof Node) document.documentElement.dataset.clientReady = 'true';
    // W112: on the map, a city or a house, never over a room or a link someone was sent.
    const stripHere = !roomMatch && !joinMatch && !joinCityMatch && !joinHouseMatch && !planMatch && !setupMatch && !COMING_BACK[path];
    if (stripHere) showSocialStrip();
    else document.querySelector('.social-strip')?.remove();
    // These pages need someone signed in, so only they save the clock.
    if (roomMatch || cityMatch || houseMatch) saveClock();
  } catch (error) {
    if (showBanned.shown || error.reason === 'banned') return;
    if (error.status === 401 || error.status === 404) {
      history.replaceState({}, '', '/');
      try {
        app.replaceChildren(await homeView());
        document.documentElement.dataset.clientReady = 'true';
        return;
      } catch (again) {
        error = again;
      }
    }
    app.replaceChildren(h('main', { class: 'home' }, h('div', { class: 'card' },
      h('h2', {}, 'That didn\'t load'),
      h('p', { class: 'muted' }, error.message),
      h('button', { type: 'button', class: 'primary', onclick: () => clientUpdates.reload() }, 'Reload app'))));
  }
}

addEventListener('popstate', route);
route();
