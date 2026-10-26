const title = document.title.toLowerCase();
const text = (document.body === null ? "" : document.body.innerText).slice(0, 65536).toLowerCase();
const challenge = [
    "verify you are human",
    "verify that you are human",
    "checking your browser",
    "performing security verification",
    "complete the security check",
    "enable javascript and cookies to continue",
    "unusual traffic from your computer network"
].some(phrase => text.includes(phrase));
const challengeTitle = ["just a moment", "access denied", "attention required", "security check"]
    .some(phrase => title.includes(phrase));
const authentication = [
    "you must be logged in to view",
    "please log in to continue",
    "please sign in to continue",
    "sign in to view this page",
    "log in to view this page"
].some(phrase => text.includes(phrase));
const paywall = [
    "subscribe to continue reading",
    "subscription required to view",
    "a subscription is required",
    "subscribe to unlock this content"
].some(phrase => text.includes(phrase));
const password = document.querySelector('input[type="password"]');
const passwordVisible = password !== null && password.getClientRects().length > 0;
let denial = null;
if (challenge || challengeTitle) {
    denial = "challenge";
} else if (authentication || passwordVisible) {
    denial = "authentication";
} else if (paywall) {
    denial = "paywall";
}
return {url: location.href, denial};
