# Installer teardown experiment
Five paired repetitions of one existing uninstall case: 8.906s baseline versus
8.901s with zombie detection in the fake service manager. Change -0.06%, 95% CI
[-0.22%, +0.58%]. CPU 0.777s versus 0.773s. WITHIN NOISE: rejected, source restored
byte for byte. The proposed five-second zombie wait was not the measured cost.
Harness and logs remain available for reproduction; no test assertion changed.
