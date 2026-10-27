// The Studio window's entry point. The modules share one `window.Capturra` object, so the import
// order is the load order.
import { followTheme } from "../design/theme";
import "../design/tokens.css";
import "../design/components.css";
import "./studio.css";
import "./ui";
import "./bridge";
import "./zoom";
import "./demo";
import "./stage";
import "./timeline";
import "./library";
import "./recorder";
import "./editor";
import "./app";

// Follow the saved appearance, including a change made from the popover or another window.
followTheme();
