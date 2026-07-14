/**
 * Google Apps Script for FRKN survey reward.
 *
 * 1. Create a Google Form with an email question.
 * 2. Open Apps Script from the form (⋮ → Scripts / Сценарии) or create at
 *    https://script.google.com and bind the trigger to the form manually.
 * 3. Paste this code, set CAMPAIGN_TOKEN and EMAIL_QUESTION_TITLE.
 * 4. Add a trigger: onFormSubmit, source "From form", event "On form submit".
 */

const API_URL = "https://api.frkn.org/surveys/reward";
const CAMPAIGN_TOKEN = "change-me";

// Set to the exact title of the email question in your form.
// Leave empty ("") if you use Google's "Collect email addresses" setting.
const EMAIL_QUESTION_TITLE = "Email";

function onFormSubmit(e) {
  let email = "";

  // Option 1: Google Forms "Collect email addresses" is enabled.
  if (e && e.response) {
    email = e.response.getRespondentEmail();
  }

  // Option 2: find by question title.
  if (!email && e && e.namedValues && EMAIL_QUESTION_TITLE) {
    const answers = e.namedValues[EMAIL_QUESTION_TITLE];
    if (answers && answers.length > 0) {
      email = answers[0];
    }
  }

  // Option 3: email is the first question.
  if (!email && e && e.values && e.values.length > 0) {
    email = e.values[0];
  }

  email = (email || "").trim().toLowerCase();

  if (!email) {
    console.error("Email not found in form submission");
    return;
  }

  const response = UrlFetchApp.fetch(API_URL, {
    method: "post",
    contentType: "application/json",
    headers: {
      Authorization: "Bearer " + CAMPAIGN_TOKEN,
    },
    payload: JSON.stringify({ email: email }),
  });

  console.log(response.getContentText());
}

/**
 * Helper to test onFormSubmit without sending a real form.
 * Select this function in the dropdown next to "Run" and press Run.
 */
function testSubmit() {
  onFormSubmit({
    namedValues: {
      Email: ["test@example.com"],
    },
  });
}
