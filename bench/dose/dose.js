const [weight, rate, sessions] = require("fs").readFileSync(0, "utf8").split(/\s+/).map(Number);
console.log(weight * rate * sessions);
