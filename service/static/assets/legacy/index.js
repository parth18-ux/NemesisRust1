const BARS = [
  ["XGBoost + graph features", .715, 1], ["Random Forest + graph", .706], ["CatBoost + graph", .700], ["LightGBM + graph", .699],
  ["LSTM (sequence)", .662], ["Transformer (sequence)", .655], ["1D-CNN (sequence)", .644], ["MLP", .574],
  ["Decision Tree + graph", .533], ["GNN (strictly causal)", .516], ["AdaBoost + graph", .501], ["XGBoost, raw fields only", .172],
  ["Logistic Regression + graph", .132], ["Isolation Forest", .004]];
document.getElementById("bars").innerHTML = BARS.map(([n, v, w]) =>
  `<div class="bar${w ? " win" : ""}"><span>${n}</span><div class="t"><div style="width:${(v / .75 * 100).toFixed(1)}%"></div></div><span class="v">${v.toFixed(3)}</span></div>`).join("");
