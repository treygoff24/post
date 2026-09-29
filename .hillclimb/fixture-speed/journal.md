# Lazy bridge fixture experiment
Four existing bridge helper cases, five paired repetitions. Eager setup median
2.168s; lazy setup 1.929s. Paired change -9.28%, 95% CI [-15.65%, -7.31%]. CPU
1.822s versus 1.624s. Below the 10% minimum effect: rejected and original test
source restored byte for byte. All four tests ran each time. No shared fixture,
assertion deletion, or production behavior change was retained.
