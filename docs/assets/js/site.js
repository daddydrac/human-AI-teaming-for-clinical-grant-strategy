const toggle = document.querySelector('.nav-toggle');
const nav = document.querySelector('.site-nav');
if (toggle && nav) {
  toggle.addEventListener('click', () => {
    const open = toggle.getAttribute('aria-expanded') === 'true';
    toggle.setAttribute('aria-expanded', String(!open));
    nav.classList.toggle('is-open', !open);
  });
}

document.querySelectorAll('pre').forEach((pre) => {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'copy-button';
  button.textContent = 'Copy';
  button.addEventListener('click', async () => {
    await navigator.clipboard.writeText(pre.innerText.replace(/^Copy\n/, ''));
    button.textContent = 'Copied';
    setTimeout(() => { button.textContent = 'Copy'; }, 1500);
  });
  pre.appendChild(button);
});

const current = location.pathname.replace(/\/$/, '');
document.querySelectorAll('.site-nav a').forEach((link) => {
  const target = new URL(link.href).pathname.replace(/\/$/, '');
  if (target && target !== '' && current.endsWith(target)) link.setAttribute('aria-current', 'page');
});
